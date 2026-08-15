//! The SFTP subsystem, backed by the real filesystem — unconfined, same as
//! any other sshd's `sftp-server`.
//!
//! Confinement is Faber's job, not this daemon's (X45.1): `SftpFiles` on
//! Faber's side resolves every path against the target's root and refuses
//! anything a `REALPATH` shows escaping it *before* sending the operation
//! over. That check only works if [`realpath`](Sftp::realpath) here tells
//! the truth — a real `std`/`tokio::fs::canonicalize`, symlinks and all —
//! which is the one method in this file that is a security boundary rather
//! than a convenience.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

/// Entries returned per `READDIR` call. The protocol allows one big batch,
/// but nothing bounds how big "big" is for a directory with a lot in it —
/// this keeps a single reply inside a packet a client won't choke on.
const READDIR_BATCH: usize = 200;

enum Open {
    File(tokio::fs::File),
    Dir { entries: Vec<File>, sent: usize },
}

#[derive(Default)]
pub struct Sftp {
    handles: HashMap<String, Open>,
    next: AtomicU64,
}

impl Sftp {
    fn new_handle(&self) -> String {
        self.next.fetch_add(1, Ordering::Relaxed).to_string()
    }
}

fn status_of(id: u32, error: &std::io::Error) -> StatusCode {
    let _ = id;
    match error.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_owned(),
        language_tag: "en-US".to_owned(),
    }
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let options: std::fs::OpenOptions = pflags.into();
        let file = tokio::fs::OpenOptions::from(options)
            .open(&filename)
            .await
            .map_err(|error| status_of(id, &error))?;
        let handle = self.new_handle();
        self.handles.insert(handle.clone(), Open::File(file));
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let Some(Open::File(file)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|error| status_of(id, &error))?;

        let mut buf = vec![0u8; len as usize];
        let mut read = 0;
        loop {
            let n = tokio::io::AsyncReadExt::read(file, &mut buf[read..])
                .await
                .map_err(|error| status_of(id, &error))?;
            if n == 0 {
                break;
            }
            read += n;
            if read == buf.len() {
                break;
            }
        }
        if read == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(read);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let Some(Open::File(file)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|error| status_of(id, &error))?;
        file.write_all(&data)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(Attrs {
            id,
            attrs: (&metadata).into(),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let Some(Open::File(file)) = self.handles.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        let metadata = file.metadata().await.map_err(|error| status_of(id, &error))?;
        Ok(Attrs {
            id,
            attrs: (&metadata).into(),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(Attrs {
            id,
            attrs: (&metadata).into(),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        apply_permissions(&path, &attrs)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        if let (Some(Open::File(file)), Some(mode)) = (self.handles.get(&handle), attrs.permissions)
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file
                    .set_permissions(std::fs::Permissions::from_mode(mode))
                    .await;
            }
        }
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let mut reader = tokio::fs::read_dir(&path)
            .await
            .map_err(|error| status_of(id, &error))?;

        let mut entries = Vec::new();
        while let Ok(Some(entry)) = reader.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let attrs = match entry.metadata().await {
                Ok(metadata) => (&metadata).into(),
                Err(_) => FileAttributes::default(),
            };
            entries.push(File::new(name, attrs));
        }

        let handle = self.new_handle();
        self.handles
            .insert(handle.clone(), Open::Dir { entries, sent: 0 });
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let Some(Open::Dir { entries, sent }) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if *sent >= entries.len() {
            return Err(StatusCode::Eof);
        }
        let end = (*sent + READDIR_BATCH).min(entries.len());
        let files = entries[*sent..end].to_vec();
        *sent = end;
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        tokio::fs::remove_file(&filename)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        tokio::fs::create_dir(&path)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        tokio::fs::remove_dir(&path)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(ok(id))
    }

    /// The one method here that is a security boundary rather than a
    /// convenience — see the module doc.
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let resolved = tokio::fs::canonicalize(&path)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(Name {
            id,
            files: vec![File::dummy(resolved.to_string_lossy().into_owned())],
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        tokio::fs::rename(&oldpath, &newpath)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(ok(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let target = tokio::fs::read_link(&path)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(Name {
            id,
            files: vec![File::dummy(target.to_string_lossy().into_owned())],
        })
    }

    #[cfg(unix)]
    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, Self::Error> {
        tokio::fs::symlink(&targetpath, &linkpath)
            .await
            .map_err(|error| status_of(id, &error))?;
        Ok(ok(id))
    }
}

async fn apply_permissions(path: &str, attrs: &FileAttributes) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(mode) = attrs.permissions {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
    }
    #[cfg(not(unix))]
    let _ = (path, attrs);
    Ok(())
}
