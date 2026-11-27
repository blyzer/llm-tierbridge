//! The sources `sync run` keeps up to date: where each comes from and how often it is refreshed.
//! `omp-bundled` is not here: it is a control source that never wins a field, so it is not part
//! of the catalog the data repo tracks.

use tierbridge_catalog::{Source, modelparams, models_dev, omp_live};

use super::state::Cadence;

/// Which script turns a checkout into a `tierbridge-snapshot/1` envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitTool {
    /// `tools/snapshot-git.mjs <sourceId>`.
    SnapshotGit,
    /// `tools/snapshot-freellmapi.mjs` (replays the migrations).
    Freellmapi,
}

#[derive(Debug, Clone, Copy)]
pub struct GitSpec {
    /// Directory name under `<cache>/repos/`.
    pub name: &'static str,
    pub url: &'static str,
    /// `None`: full history (`router-for-me.models` needs it for its list of retired models).
    pub depth: Option<u32>,
    pub tool: GitTool,
}

#[derive(Debug, Clone, Copy)]
pub enum Origin {
    /// Downloaded with one GET.
    Http(&'static str),
    /// omp's cache of the CLIProxyAPI model list (`<agent dir>/cliproxyapi-models.json`).
    OmpCache,
    /// `omp models <provider> --json` through `snapshot omp-live`.
    OmpLive(&'static str),
    Git(GitSpec),
}

#[derive(Debug, Clone, Copy)]
pub struct SourceSpec {
    pub source: Source,
    pub cadence: Cadence,
    pub origin: Origin,
}

impl SourceSpec {
    pub fn id(&self) -> &'static str {
        self.source.id()
    }
}

/// Every tracked source, in the order they are listed in reports.
pub fn registry() -> Vec<SourceSpec> {
    let git = |source, name, url, depth, tool| SourceSpec {
        source,
        cadence: Cadence::OnChange,
        origin: Origin::Git(GitSpec {
            name,
            url,
            depth,
            tool,
        }),
    };
    vec![
        SourceSpec {
            source: Source::ModelsDev,
            cadence: Cadence::Daily,
            origin: Origin::Http(models_dev::API_URL),
        },
        SourceSpec {
            source: Source::ModelparamsDev,
            cadence: Cadence::Weekly,
            origin: Origin::Http(modelparams::API_URL),
        },
        SourceSpec {
            source: Source::CliproxyapiCache,
            cadence: Cadence::Daily,
            origin: Origin::OmpCache,
        },
        SourceSpec {
            source: Source::OmpLiveEasycliproxy,
            cadence: Cadence::Daily,
            origin: Origin::OmpLive(omp_live::EASYCLIPROXY),
        },
        SourceSpec {
            source: Source::OmpLiveDsh,
            cadence: Cadence::Daily,
            origin: Origin::OmpLive(omp_live::DSH),
        },
        git(
            Source::RouterModels,
            "router-for-me.models",
            "https://github.com/router-for-me/models",
            None,
            GitTool::SnapshotGit,
        ),
        git(
            Source::Dsh,
            "dsh",
            "https://github.com/Ebony-Vinyl/dsh-our-free-model",
            Some(1),
            GitTool::SnapshotGit,
        ),
        git(
            Source::Freellmapi,
            "freellmapi",
            "https://github.com/tashfeenahmed/freellmapi",
            Some(1),
            GitTool::Freellmapi,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_source_but_omp_bundled_is_tracked_once() {
        let ids: Vec<&str> = registry().iter().map(SourceSpec::id).collect();
        for s in Source::ALL {
            let tracked = ids.iter().filter(|i| **i == s.id()).count();
            assert_eq!(tracked, usize::from(s != Source::OmpBundled), "{}", s.id());
        }
    }

    #[test]
    fn cadences_follow_the_design() {
        let cadence = |s: Source| {
            registry()
                .into_iter()
                .find(|r| r.source == s)
                .unwrap()
                .cadence
        };
        assert_eq!(cadence(Source::ModelsDev), Cadence::Daily);
        assert_eq!(cadence(Source::ModelparamsDev), Cadence::Weekly);
        assert_eq!(cadence(Source::OmpLiveDsh), Cadence::Daily);
        assert_eq!(cadence(Source::CliproxyapiCache), Cadence::Daily);
        for s in [Source::RouterModels, Source::Dsh, Source::Freellmapi] {
            assert_eq!(cadence(s), Cadence::OnChange);
        }
    }
}
