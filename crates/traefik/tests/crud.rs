//! CRUD against a real file, checking both halves of every mutation: the
//! entry set the caller sees, and the document Traefik will read.

use std::path::Path;

use tempfile::TempDir;
use traefik::{Config, Domain, Entry, Placement, Target, Traefik};

fn manager(dir: &TempDir) -> Traefik {
    Traefik::new(Config::new(dir.path().join("faber.yml"), Placement::Host))
}

fn domain(value: &str) -> Domain {
    Domain::new(value).unwrap()
}

fn entry(value: &str, target: Target) -> Entry {
    Entry::new(domain(value), target)
}

/// The file is read as YAML by Traefik; here it is read as JSON, which is the
/// same bytes minus the leading comment line.
fn document(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap();
    let body = text
        .split_once('\n')
        .expect("banner line")
        .1
        .trim_start()
        .to_owned();
    serde_json::from_str(&body).unwrap()
}

fn upstream(doc: &serde_json::Value, name: &str) -> String {
    doc["http"]["services"][name]["loadBalancer"]["servers"][0]["url"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn create_read_update_delete_roundtrip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faber.yml");
    let traefik = manager(&dir);

    traefik
        .create(entry(
            "app.example.com",
            Target::container("web", 8080).unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(traefik.list().await.len(), 1);
    assert_eq!(
        traefik
            .get(&domain("app.example.com"))
            .await
            .unwrap()
            .target,
        Target::container("web", 8080).unwrap()
    );
    assert_eq!(
        upstream(&document(&path), "faber-app.example.com"),
        "http://web:8080"
    );

    traefik
        .update(&domain("app.example.com"), Target::host(3000).unwrap())
        .await
        .unwrap();
    assert_eq!(
        upstream(&document(&path), "faber-app.example.com"),
        "http://127.0.0.1:3000"
    );

    assert!(traefik.delete(&domain("app.example.com")).await.unwrap());
    assert!(traefik.list().await.is_empty());
    let doc = document(&path);
    assert!(doc["http"]["routers"].as_object().unwrap().is_empty());
    assert!(doc["http"]["services"].as_object().unwrap().is_empty());
}

#[tokio::test]
async fn create_conflicts_and_put_does_not() {
    let dir = TempDir::new().unwrap();
    let traefik = manager(&dir);
    let first = entry("app.example.com", Target::host(3000).unwrap());
    let second = entry("app.example.com", Target::host(3001).unwrap());

    traefik.create(first).await.unwrap();
    let conflict = traefik.create(second.clone()).await.unwrap_err();
    assert!(matches!(conflict, traefik::Error::AlreadyExists { .. }));
    // The rejected write left the entry untouched.
    assert_eq!(
        traefik
            .get(&domain("app.example.com"))
            .await
            .unwrap()
            .target,
        Target::host(3000).unwrap()
    );

    traefik.put(second).await.unwrap();
    assert_eq!(
        traefik
            .get(&domain("app.example.com"))
            .await
            .unwrap()
            .target,
        Target::host(3001).unwrap()
    );
}

#[tokio::test]
async fn update_and_delete_of_an_absent_domain() {
    let dir = TempDir::new().unwrap();
    let traefik = manager(&dir);

    let missing = traefik
        .update(&domain("nope.example.com"), Target::host(3000).unwrap())
        .await
        .unwrap_err();
    assert!(matches!(missing, traefik::Error::NotFound { .. }));

    assert!(!traefik.delete(&domain("nope.example.com")).await.unwrap());
}

#[tokio::test]
async fn replace_publishes_the_whole_set_and_drops_what_is_gone() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faber.yml");
    let traefik = manager(&dir);

    traefik
        .create(entry("old.example.com", Target::host(3000).unwrap()))
        .await
        .unwrap();

    traefik
        .replace([
            entry("a.example.com", Target::container("web", 80).unwrap()),
            entry("b.example.com", Target::host(3001).unwrap()),
        ])
        .await
        .unwrap();

    let domains: Vec<String> = traefik
        .list()
        .await
        .into_iter()
        .map(|entry| entry.domain.to_string())
        .collect();
    assert_eq!(domains, ["a.example.com", "b.example.com"]);

    let doc = document(&path);
    let routers = doc["http"]["routers"].as_object().unwrap();
    assert_eq!(routers.len(), 2);
    assert!(!routers.contains_key("faber-old.example.com"));

    // A restart with an empty database empties the file rather than leaving
    // a previous process's routers live.
    traefik.replace([]).await.unwrap();
    assert!(
        document(&path)["http"]["routers"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn replace_rejects_a_duplicated_domain() {
    let dir = TempDir::new().unwrap();
    let traefik = manager(&dir);

    let err = traefik
        .replace([
            entry("a.example.com", Target::host(3000).unwrap()),
            entry("a.example.com", Target::host(3001).unwrap()),
        ])
        .await
        .unwrap_err();
    assert!(matches!(err, traefik::Error::AlreadyExists { .. }));
    assert!(traefik.list().await.is_empty());
}

#[tokio::test]
async fn domains_that_a_slug_would_collapse_stay_separate() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faber.yml");
    let traefik = manager(&dir);

    traefik
        .create(entry("example.com", Target::host(3000).unwrap()))
        .await
        .unwrap();
    traefik
        .create(entry("example-com", Target::host(3001).unwrap()))
        .await
        .unwrap();

    let doc = document(&path);
    assert_eq!(doc["http"]["routers"].as_object().unwrap().len(), 2);
    assert_eq!(upstream(&doc, "faber-example.com"), "http://127.0.0.1:3000");
    assert_eq!(upstream(&doc, "faber-example-com"), "http://127.0.0.1:3001");
}

#[tokio::test]
async fn host_targets_follow_the_placement_but_container_targets_do_not() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("faber.yml");
    let traefik = Traefik::new(Config::new(
        &path,
        Placement::docker("host.docker.internal").unwrap(),
    ));

    traefik
        .replace([
            entry("host.example.com", Target::host(3000).unwrap()),
            entry("cont.example.com", Target::container("web", 8080).unwrap()),
        ])
        .await
        .unwrap();

    let doc = document(&path);
    assert_eq!(
        upstream(&doc, "faber-host.example.com"),
        "http://host.docker.internal:3000"
    );
    assert_eq!(upstream(&doc, "faber-cont.example.com"), "http://web:8080");
}

#[tokio::test]
async fn the_temporary_file_does_not_outlive_the_write() {
    let dir = TempDir::new().unwrap();
    let traefik = manager(&dir);
    traefik
        .create(entry("app.example.com", Target::host(3000).unwrap()))
        .await
        .unwrap();

    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["faber.yml"]);
}

#[tokio::test]
async fn a_write_failure_leaves_the_entry_set_alone() {
    let dir = TempDir::new().unwrap();
    // A directory that does not exist: the render succeeds, the write cannot.
    let traefik = Traefik::new(Config::new(
        dir.path().join("absent").join("faber.yml"),
        Placement::Host,
    ));

    let err = traefik
        .create(entry("app.example.com", Target::host(3000).unwrap()))
        .await
        .unwrap_err();
    assert!(matches!(err, traefik::Error::Io { .. }));
    assert!(traefik.list().await.is_empty());
}
