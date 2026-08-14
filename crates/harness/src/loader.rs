//! A module loader that resolves exactly two specifiers and refuses
//! everything else.
//!
//! `abstract.md` §7: "Import control *is* the sandbox." A harness may reach
//! its own module and the capability-object builder Core injects — nothing
//! else. No filesystem, no network, no other harness's code.

use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleResolveResponse,
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind,
};
use deno_error::JsErrorBox;

pub const HARNESS_SPECIFIER: &str = "harness:main";
// Deliberately not an `ext:` specifier: deno_core only allows `ext:` modules
// to be imported from other `ext:`/`node:` modules, and the bootstrap module
// (`harness:bootstrap`) is neither.
pub const CONTEXT_SPECIFIER: &str = "faber:context.js";
/// Not resolved through this loader — the bootstrap module is handed to
/// `load_main_es_module_from_code` directly by `runtime.rs`, since its
/// source is generated fresh per run (it embeds the JSON-encoded input).
pub const BOOTSTRAP_SPECIFIER: &str = "harness:bootstrap";

const CONTEXT_SOURCE: &str = include_str!("context.js");

/// The harness's own source, embedded at construction time so no path ever
/// resolves to a filesystem read.
pub struct HarnessLoader {
    pub harness_source: String,
}

impl ModuleLoader for HarnessLoader {
    fn resolve(
        &self,
        specifier: &str,
        _referrer: &str,
        _kind: ResolutionKind,
    ) -> ModuleResolveResponse {
        match specifier {
            // The main module's own specifier resolves through here even
            // though its source was already supplied to
            // `load_main_es_module_from_code` — `load()` is never actually
            // called for it.
            HARNESS_SPECIFIER | CONTEXT_SPECIFIER | BOOTSTRAP_SPECIFIER => {
                ModuleSpecifier::parse(specifier).map_err(|error| {
                    ModuleLoaderError::from(JsErrorBox::generic(format!(
                        "harness module specifier `{specifier}` is not a valid URL: {error}"
                    )))
                })
            }
            other => Err(ModuleLoaderError::from(JsErrorBox::generic(format!(
                "import of `{other}` refused: a harness may only import its own module; \
                 import control is the sandbox (abstract.md §7)"
            )))),
        }
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let specifier = module_specifier.as_str();
        let source = match specifier {
            HARNESS_SPECIFIER => self.harness_source.clone(),
            CONTEXT_SPECIFIER => CONTEXT_SOURCE.to_string(),
            other => {
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::from(
                    JsErrorBox::generic(format!("refused to load `{other}`")),
                )));
            }
        };

        ModuleLoadResponse::Sync(Ok(ModuleSource::new(
            ModuleType::JavaScript,
            ModuleSourceCode::String(source.into()),
            module_specifier,
            None,
        )))
    }
}
