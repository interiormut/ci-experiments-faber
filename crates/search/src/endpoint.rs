//! Appending a path to a base URL without discarding the base's own path.
//!
//! `Url::join` with an absolute path throws away whatever prefix the base
//! carried, which silently misroutes exactly the deployments that make the
//! base configurable in the first place: an instance published at
//! `https://example.org/searx` has to stay under `/searx`.

use url::Url;

use crate::error::{Error, Result};

pub(crate) fn endpoint<'a>(
    mut base: Url,
    segments: impl IntoIterator<Item = &'a str>,
) -> Result<Url> {
    {
        let mut path = base
            .path_segments_mut()
            .map_err(|()| Error::Config("base URL must be http or https".into()))?;
        // A base written with a trailing slash carries an empty last segment;
        // popping it keeps both spellings from producing `/searx//search`.
        path.pop_if_empty().extend(segments);
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_base_path() {
        let base = Url::parse("https://example.org/searx").unwrap();
        assert_eq!(
            endpoint(base, ["search"]).unwrap().as_str(),
            "https://example.org/searx/search"
        );
    }

    #[test]
    fn trailing_slash_does_not_double() {
        let base = Url::parse("https://example.org/searx/").unwrap();
        assert_eq!(
            endpoint(base, ["search"]).unwrap().as_str(),
            "https://example.org/searx/search"
        );
    }

    #[test]
    fn bare_host_works_either_way() {
        for spelling in ["https://example.org", "https://example.org/"] {
            let base = Url::parse(spelling).unwrap();
            assert_eq!(
                endpoint(base, ["search"]).unwrap().as_str(),
                "https://example.org/search"
            );
        }
    }
}
