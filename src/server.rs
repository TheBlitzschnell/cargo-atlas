//! `cargo atlas serve`: the questions as MCP tools, over stdin and stdout.
//!
//! An AI assistant such as Claude Code starts this process and calls its
//! tools. Each answer is the text the command line prints for the same
//! question.
//!
//! The graph stays in memory between calls. Before each answer the server
//! checks whether any file changed since the graph was built (see
//! `freshness`). If one did, the answer starts with a note naming the files,
//! and a rebuild starts in the background; later answers use the new graph
//! once it is ready. The `refresh` tool rebuilds and waits.
//!
//! Nothing runs before the first call. An assistant may start the server in
//! every project, Rust or not, and indexing costs seconds to a minute of CPU.
//!
//! This is the only async code in the crate. rmcp, the MCP library, runs on
//! tokio; the queries are plain functions that return in milliseconds, and
//! builds run on a thread meant for blocking work.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::cargo_meta::{self, Workspace};
use crate::model::Features;
use crate::query::{Ambiguous, Atlas};
use crate::{freshness, pipeline};

/// Answers longer than this are cut. Claude Code warns when a tool returns
/// more than about 10,000 tokens; 24,000 characters is roughly 6,000.
const MAX_ANSWER_CHARS: usize = 24_000;

/// Shown to the assistant when it connects.
const INSTRUCTIONS: &str = "\
cargo-atlas answers questions about the structure of this Rust workspace from rust-analyzer's \
resolved index: who calls a function, what it calls, which types implement a trait, how two \
items connect, which tests reach a function, and where unsafe code is. Prefer these tools to \
grep for such questions: grep finds every method named `parse`, while these tools know which \
one each call resolves to. Answers give file:line for everything, so read only those lines.

Every link is labeled EXACT (resolved by rust-analyzer), CANDIDATE (one of several possible \
targets, such as the impls behind a `dyn Trait` call) or SYNTAX (read from the source text, \
such as derives).

Name an item by its name (`parse`), its path (`JsonReader::parse`, optionally with the crate \
first) or the file:line of its definition (`src/json_reader.rs:10`). If a name matches several \
items, the answer lists them; ask again with one of the listed paths.

The first call indexes the workspace if it has no graph yet, which can take a minute on a large \
workspace. When files change, answers name the changed files and a rebuild runs in the \
background; call `refresh` to wait for it.";

/// Runs the server until the assistant closes stdin. `features`, when given,
/// is what every build uses; otherwise a rebuild keeps the features of the
/// graph it replaces.
pub fn run(dir: &Path, features: Option<Features>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let result = runtime.block_on(async {
        let service = AtlasServer::new(dir.to_path_buf(), features)
            .serve(rmcp::transport::stdio())
            .await
            .context("starting the MCP server")?;
        service.waiting().await.context("running the MCP server")?;
        Ok(())
    });
    // A rebuild may still be running. Don't make the assistant wait for it
    // to exit; graph.json is replaced in one step, so it is never left half-written.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ItemArgs {
    // `schemars(description)` rather than a doc comment, so the text reaches
    // the assistant as one paragraph instead of with the source's line breaks.
    #[schemars(
        description = "The item: a name (`parse`), a path (`JsonReader::parse`, \
        optionally with the crate first), or the file:line of its definition \
        (`src/json_reader.rs:10`)."
    )]
    item: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PathArgs {
    /// Where the path starts: a name, a path or a file:line.
    from: String,
    /// Where the path ends: a name, a path or a file:line.
    to: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// Part of the name or path, in any case.
    text: String,
    #[schemars(
        description = "Only one kind of item: function, method, trait method, \
        struct, enum, trait, type alias, module, const, static, macro, crate."
    )]
    kind: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct UnsafeArgs {
    #[schemars(
        description = "Optional. A crate, module, type or function to look inside, \
        named like any other item. Without it: all unsafe code in the workspace."
    )]
    item: Option<String>,
    #[schemars(
        description = "Optional. True for only the sites that lack their `// SAFETY:` \
        comment or `# Safety` section."
    )]
    missing_only: Option<bool>,
}

#[derive(Clone)]
pub struct AtlasServer {
    shared: Arc<Shared>,
    tool_router: ToolRouter<Self>,
}

struct Shared {
    dir: PathBuf,
    /// Cargo features from the command line, if any.
    features: Option<Features>,
    state: Mutex<State>,
    /// Held while a graph is loaded or built, so only one build runs at a time.
    build_lock: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    /// Read with `cargo metadata` on first use.
    workspace: Option<Workspace>,
    atlas: Option<Arc<Atlas>>,
    /// graph.json's modification time when this server last loaded or wrote it.
    graph_mtime: Option<SystemTime>,
    /// A background rebuild is running.
    rebuilding: bool,
    /// How long the last build took, to tell the assistant what to expect.
    last_build_seconds: Option<f64>,
    /// Why the last background rebuild failed. Automatic rebuilds stop until
    /// a `refresh` succeeds, so a broken setup doesn't rebuild on every call.
    last_error: Option<String>,
    /// Finished builds, so `refresh` can tell that one finished while it waited.
    builds: u64,
}

#[tool_router]
impl AtlasServer {
    fn new(dir: PathBuf, features: Option<Features>) -> Self {
        AtlasServer {
            shared: Arc::new(Shared {
                dir,
                features,
                state: Mutex::new(State::default()),
                build_lock: tokio::sync::Mutex::new(()),
            }),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Who calls a Rust function, method, macro or constant, with the file:line of \
        each call. Resolved by rust-analyzer, so `parse` on one type is never mixed up with `parse` \
        on another. A call through `dyn Trait` or a generic lists the impls that may run, marked \
        CANDIDATE.",
        annotations(read_only_hint = true)
    )]
    async fn callers(&self, Parameters(ItemArgs { item }): Parameters<ItemArgs>) -> CallToolResult {
        self.answer(move |atlas| atlas.callers(&item)).await
    }

    #[tool(
        description = "What a Rust function or method calls, with the file:line of each call. \
        Calls through a trait list the impls that may run, marked CANDIDATE.",
        annotations(read_only_hint = true)
    )]
    async fn callees(&self, Parameters(ItemArgs { item }): Parameters<ItemArgs>) -> CallToolResult {
        self.answer(move |atlas| atlas.callees(&item)).await
    }

    #[tool(
        description = "For a trait: the types that implement it. For a type: the traits it \
        implements and derives.",
        annotations(read_only_hint = true)
    )]
    async fn impls(&self, Parameters(ItemArgs { item }): Parameters<ItemArgs>) -> CallToolResult {
        self.answer(move |atlas| atlas.impls(&item)).await
    }

    #[tool(
        description = "The shortest chain of calls, type uses and impls between two items, with \
        the file:line of each link. Answers questions like 'how does main reach this function?'",
        annotations(read_only_hint = true)
    )]
    async fn path(
        &self,
        Parameters(PathArgs { from, to }): Parameters<PathArgs>,
    ) -> CallToolResult {
        self.answer(move |atlas| atlas.path(&from, &to)).await
    }

    #[tool(
        description = "Everything about one item: kind, location, signature, whether it is a test, \
        the unsafe code in it, and every link in and out.",
        annotations(read_only_hint = true)
    )]
    async fn explain(&self, Parameters(ItemArgs { item }): Parameters<ItemArgs>) -> CallToolResult {
        self.answer(move |atlas| atlas.explain(&item)).await
    }

    #[tool(
        description = "Find items whose name or path contains some text, closest matches first, \
        with kind and location. Use it to get an item's exact path before asking the other tools.",
        annotations(read_only_hint = true)
    )]
    async fn search(
        &self,
        Parameters(SearchArgs { text, kind }): Parameters<SearchArgs>,
    ) -> CallToolResult {
        self.answer(move |atlas| atlas.search(&text, kind.as_deref()))
            .await
    }

    #[tool(
        description = "The tests that reach a function through calls, directly or through other \
        functions, and the `cargo test` command that runs just those tests. Use it after changing \
        a function to know which tests to run. For a type or trait: tests that reach its methods.",
        annotations(read_only_hint = true)
    )]
    async fn tests(&self, Parameters(ItemArgs { item }): Parameters<ItemArgs>) -> CallToolResult {
        self.answer(move |atlas| atlas.tests(&item)).await
    }

    #[tool(
        description = "Unsafe code: every `unsafe` block, fn, impl and trait, and whether each has \
        its `// SAFETY:` comment (blocks and impls) or `# Safety` doc section (fns and traits). \
        With an item: the unsafe code inside it, and for a function also the unsafe code it \
        reaches through calls.",
        annotations(read_only_hint = true)
    )]
    async fn unsafe_code(
        &self,
        Parameters(UnsafeArgs { item, missing_only }): Parameters<UnsafeArgs>,
    ) -> CallToolResult {
        let missing_only = missing_only.unwrap_or(false);
        self.answer(move |atlas| atlas.unsafe_code(item.as_deref(), missing_only))
            .await
    }

    #[tool(
        description = "Rebuild the graph now and wait for it: seconds on a small workspace, up to \
        a minute on a large one. The other tools already notice changed files and rebuild in the \
        background; call this when an answer says the graph is out of date and you need answers \
        about the new code."
    )]
    async fn refresh(&self) -> CallToolResult {
        match self.rebuild_and_wait().await {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(error) => {
                let message = format!("{error:#}");
                self.state().last_error = Some(message.clone());
                CallToolResult::error(vec![ContentBlock::text(message)])
            }
        }
    }
}

// The router is built once, in `new`, and kept in the struct.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for AtlasServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "cargo-atlas",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }
}

impl AtlasServer {
    fn state(&self) -> MutexGuard<'_, State> {
        // A panic while the lock was held leaves plain data behind; keep using it.
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Answers a question from the current graph. Errors become error
    /// results the assistant can read, except "several items match", which
    /// is an ordinary answer: the next step is to pick one from the list.
    async fn answer(&self, question: impl FnOnce(&Atlas) -> Result<String>) -> CallToolResult {
        let (atlas, note) = match self.current_graph().await {
            Ok(found) => found,
            Err(error) => {
                return CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]);
            }
        };
        let (text, failed) = match question(&atlas) {
            Ok(text) => (text, false),
            Err(error) if error.is::<Ambiguous>() => (error.to_string(), false),
            Err(error) => (format!("{error:#}"), true),
        };
        let content = vec![ContentBlock::text(cut(format!(
            "{}{text}",
            note.unwrap_or_default()
        )))];
        if failed {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
    }

    /// The graph to answer from, and a note to put before the answer when
    /// files changed since it was built.
    async fn current_graph(&self) -> Result<(Arc<Atlas>, Option<String>)> {
        let workspace = self.workspace()?;
        self.load_on_first_use(&workspace).await?;
        self.reload_if_rebuilt_elsewhere(&workspace);
        let atlas = self.state().atlas.clone().context("no graph loaded")?;
        let changed = freshness::changed(&workspace.root, &atlas.graph().files);
        if changed.is_empty() {
            return Ok((atlas, None));
        }
        self.start_rebuild(&workspace);
        Ok((atlas, Some(self.stale_note(&changed))))
    }

    fn workspace(&self) -> Result<Workspace> {
        if let Some(workspace) = &self.state().workspace {
            return Ok(workspace.clone());
        }
        let workspace = cargo_meta::load(&self.shared.dir).with_context(|| {
            format!(
                "{} is not a Cargo workspace, or `cargo metadata` failed there",
                self.shared.dir.display()
            )
        })?;
        self.state().workspace = Some(workspace.clone());
        Ok(workspace)
    }

    /// Loads graph.json, or builds it if it is missing or in an old format.
    /// The first call waits for this; the calls after it don't.
    async fn load_on_first_use(&self, workspace: &Workspace) -> Result<()> {
        if self.state().atlas.is_some() {
            return Ok(());
        }
        let _building = self.shared.build_lock.lock().await;
        if self.state().atlas.is_some() {
            // Another call loaded it while this one waited for the lock.
            return Ok(());
        }
        let path = pipeline::graph_path(workspace);
        match Atlas::load(&path) {
            Ok(atlas) if self.wants(&atlas) => {
                let mut state = self.state();
                state.atlas = Some(Arc::new(atlas));
                state.graph_mtime = modified(&path);
                Ok(())
            }
            // Missing, in an old format, or built with other features than
            // the ones given to `serve`.
            _ => self.build_now(workspace.clone()).await.map(|_| ()),
        }
    }

    /// False for a graph built with other features than the ones given to `serve`.
    fn wants(&self, atlas: &Atlas) -> bool {
        self.shared
            .features
            .as_ref()
            .is_none_or(|f| *f == atlas.graph().features)
    }

    /// Switches to graph.json if something else wrote it since this server
    /// last read it, such as `cargo atlas build` in a terminal.
    fn reload_if_rebuilt_elsewhere(&self, workspace: &Workspace) {
        let path = pipeline::graph_path(workspace);
        let on_disk = modified(&path);
        if on_disk.is_none() || on_disk == self.state().graph_mtime {
            return;
        }
        let loaded = Atlas::load(&path).ok().filter(|atlas| self.wants(atlas));
        let mut state = self.state();
        // Remember this version even if it wasn't used, so it isn't read again on every call.
        state.graph_mtime = on_disk;
        if let Some(atlas) = loaded {
            state.atlas = Some(Arc::new(atlas));
        }
    }

    /// Starts a rebuild in the background, unless one is running or the
    /// last one failed.
    fn start_rebuild(&self, workspace: &Workspace) {
        {
            let mut state = self.state();
            if state.rebuilding || state.last_error.is_some() {
                return;
            }
            state.rebuilding = true;
        }
        let server = self.clone();
        let workspace = workspace.clone();
        tokio::spawn(async move {
            let result = {
                let _building = server.shared.build_lock.lock().await;
                server.build_now(workspace).await
            };
            let mut state = server.state();
            state.rebuilding = false;
            if let Err(error) = result {
                state.last_error = Some(format!("{error:#}"));
            }
        });
    }

    /// The `refresh` tool: waits for any running build, then builds, unless
    /// a build that finished meanwhile already matches the files.
    async fn rebuild_and_wait(&self) -> Result<String> {
        let workspace = self.workspace()?;
        let builds_before = self.state().builds;
        let _building = self.shared.build_lock.lock().await;
        let finished_meanwhile = self.state().builds > builds_before;
        // Cloned first, so the lock isn't held while the files are checked.
        let current = self.state().atlas.clone();
        let current = current
            .filter(|atlas| freshness::changed(&workspace.root, &atlas.graph().files).is_empty());
        if let (true, Some(atlas)) = (finished_meanwhile, current) {
            return Ok(format!(
                "A rebuild finished while this call waited, and it matches the files: {} items.",
                atlas.graph().nodes.len()
            ));
        }
        self.build_now(workspace).await
    }

    /// Builds the graph on a blocking thread and switches to it. The caller
    /// must hold `build_lock`.
    async fn build_now(&self, workspace: Workspace) -> Result<String> {
        let path = pipeline::graph_path(&workspace);
        let features = self.shared.features.clone().unwrap_or_else(|| {
            let current = self.state().atlas.clone();
            current
                .map(|a| a.graph().features.clone())
                .unwrap_or_default()
        });
        let built = tokio::task::spawn_blocking(move || pipeline::run(&workspace, &features))
            .await
            .context("the build thread stopped")??;
        let mut text = built.summary();
        for warning in &built.warnings {
            text.push_str(&format!("\nWarning: {warning}"));
        }
        let mut state = self.state();
        state.last_build_seconds = Some(built.seconds);
        state.atlas = Some(Arc::new(Atlas::new(built.graph)));
        state.graph_mtime = modified(&path);
        state.last_error = None;
        state.builds += 1;
        Ok(text)
    }

    fn stale_note(&self, changed: &[String]) -> String {
        let state = self.state();
        let status = match (
            &state.last_error,
            state.rebuilding,
            state.last_build_seconds,
        ) {
            (Some(error), _, _) => {
                // The first line only: the rest can be twenty lines of rust-analyzer output.
                let first_line = error.lines().next().unwrap_or_default();
                format!(
                    "The last rebuild failed: {first_line} Call `refresh` to try again \
                     and see the whole error."
                )
            }
            (None, true, Some(seconds)) => format!(
                "A rebuild is running (the last build took {seconds:.0} s); \
                 call `refresh` to wait for it."
            ),
            (None, true, None) => "A rebuild is running; call `refresh` to wait for it.".into(),
            (None, false, _) => "Call `refresh` to rebuild.".into(),
        };
        format!(
            "Note: {} since the graph was built, so answers about code there may be out of \
             date. {status}\n\n",
            freshness::describe(changed)
        )
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Cuts an answer that is too long at a line break, and says so.
fn cut(text: String) -> String {
    if text.len() <= MAX_ANSWER_CHARS {
        return text;
    }
    let mut end = MAX_ANSWER_CHARS;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').unwrap_or(end);
    format!(
        "{}\n... (answer cut at {MAX_ANSWER_CHARS} characters; ask about a narrower item)\n",
        &text[..end]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_answers_are_cut_at_a_line_break() {
        let line = "x".repeat(99) + "\n";
        let long = line.repeat(1_000);
        let cut = cut(long);
        assert!(cut.len() < MAX_ANSWER_CHARS + 200);
        assert!(cut.ends_with("ask about a narrower item)\n"));
        let body = cut.lines().next().unwrap();
        assert_eq!(body.len(), 99, "cut in the middle of a line");
        assert_eq!(super::cut("short".into()), "short");
    }
}
