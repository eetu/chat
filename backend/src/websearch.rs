//! Live web retrieval for the chat path.
//!
//! Search runs in-process via the `daedra` crate, which fans a query out
//! across a chain of backends (Bing RSS, Google News, Marginalia, mwmbl,
//! Wikipedia, Hacker News, StackExchange, GitHub, Wiby, DDG Instant) and
//! falls back down the chain when one is blocked or rate-limited. No API
//! key, no upstream service to deploy, nothing leaving the LAN except
//! the query itself.
//!
//! Search results carry only a short snippet, which is thin ground for
//! an answer, so the top few are fetched and reduced to article text by
//! [`crate::safefetch`] — deliberately our own fetcher rather than the
//! crate's, because that path takes URLs influenced by chat input and is
//! the one place an SSRF bug would matter.
//!
//! Shape mirrors the RAG path in `handlers::chat`: gather a handful of
//! passages, fold them into one `system` message, and report the sources
//! back to the client.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use daedra::tools::backend::SearchProvider;
use daedra::types::{SafeSearchLevel, SearchArgs, SearchOptions};
use futures_util::StreamExt;

use crate::ollama::ChatMessage;
use crate::AppState;

/// The backend chain, built once and reused.
///
/// Not merely an allocation saving: the provider owns the per-backend
/// circuit breakers and rate limiters, so rebuilding it per turn would
/// reset that memory every time and keep hammering a backend that just
/// served a CAPTCHA.
///
/// Note this is `SearchProvider::auto()`, not `search::perform_search` —
/// the latter is the plain DuckDuckGo path and ignores backend
/// selection entirely.
static PROVIDER: OnceLock<SearchProvider> = OnceLock::new();

fn provider() -> &'static SearchProvider {
    PROVIDER.get_or_init(SearchProvider::auto)
}

/// Characters of a single result's text kept when building the context
/// block. Fetched pages are far longer than this; the budget is what
/// keeps five results inside a small local model's window.
const RESULT_CHAR_BUDGET: usize = 1200;

/// Backends excluded by default. These three scrape the HTML SERPs of
/// Google, Bing and DuckDuckGo, which breaks those engines' terms of
/// service and earns a CAPTCHA sooner or later. The RSS and public-API
/// backends cover general queries without either problem. Override with
/// `WEB_SEARCH_ALLOW_SCRAPERS=1` if you decide otherwise.
const SCRAPER_BACKENDS: [&str; 3] = ["google", "bing", "duckduckgo"];

/// Backends skipped unless `WEB_SEARCH_EXCLUDE_BACKENDS` says otherwise.
///
/// `marginalia` is here for a measured reason, not a preference. daedra
/// builds its public URL path-style — `/public/search/<query>` — and
/// that shape does not answer: it hangs until the client's own 30 s
/// timeout, twice, because a transient error earns one retry. Since
/// every backend is awaited before the aggregate returns, that was 60 s
/// added to *every* search while the other nine finished inside one
/// second. The query-style URL 404s immediately, so the endpoint moved
/// and the crate did not follow.
///
/// Worth revisiting: it is a genuinely good independent index, and a
/// `MARGINALIA_API_KEY` selects a different base URL that may well
/// work. Drop it from the exclusion list and time a search.
pub const DEFAULT_EXCLUDED: [&str; 1] = ["marginalia"];

/// Turns of prior conversation shown to the query-rewrite model, and
/// how much of each. Enough for pronoun resolution, not enough to make
/// the rewrite call expensive.
const QUERY_HISTORY_TURNS: usize = 4;
const QUERY_HISTORY_CHARS: usize = 400;

/// Upper bound on a rewritten query. Search backends truncate long
/// queries anyway; this mostly guards against a model that ignores the
/// instruction and returns a paragraph.
const MAX_QUERY_CHARS: usize = 200;

/// Pages fetched at once during [`hydrate`]. Bounded by memory, not by
/// politeness: each in-flight page holds its bytes plus the DOM built
/// from them, and the container's `MemoryMax` is what that has to fit
/// inside. Three at 512 KB apiece is comfortable at a 128 MiB cap.
const FETCH_CONCURRENCY: usize = 3;

/// Wall-clock ceiling on the whole fetch phase. Nothing streams to the
/// user until retrieval returns, so this is the longest the composer
/// can sit silent before the answer starts. Results that miss it keep
/// their search snippets.
const HYDRATE_BUDGET: std::time::Duration = std::time::Duration::from_secs(12);

/// Ceiling on the search phase. Generous, because it should never fire:
/// with the stalled backend excluded, a real search aggregates in about
/// a second. It exists so the next backend to go dark costs a bounded
/// wait instead of a minute.
const SEARCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum WebSearchError {
    #[error("web search is not enabled")]
    Disabled,
    #[error("every search backend failed: {0}")]
    AllBackendsFailed(String),
    #[error("search returned nothing usable")]
    Empty,
    #[error("search backends did not answer in time")]
    TimedOut,
    #[error("page fetch failed: {0}")]
    Fetch(#[from] crate::safefetch::FetchError),
}

/// One retrieved source. `content` is a snippet when it came straight
/// from a search backend, or article text when the page was fetched.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub content: String,
    /// True once the page body has been fetched and extracted. Drives
    /// nothing user-visible; useful when reading debug logs to tell a
    /// thin answer caused by a failed fetch from a thin snippet.
    pub fetched: bool,
}

/// True when web search is turned on for this deploy. The composer's
/// affordance and the chat handler's retrieval branch both gate on
/// this, so a client that sends the flag against a server with the
/// feature off is simply ignored.
pub fn is_configured(state: &AppState) -> bool {
    state.settings.web_search_enabled
}

/// Run a search and return ranked results, snippets only.
pub async fn search(
    state: &AppState,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>, WebSearchError> {
    if !state.settings.web_search_enabled {
        return Err(WebSearchError::Disabled);
    }
    let mut excluded: Vec<String> = state.settings.web_search_exclude_backends.clone();
    if !state.settings.web_search_allow_scrapers {
        excluded.extend(SCRAPER_BACKENDS.iter().map(|s| s.to_string()));
    }
    let exclude = if excluded.is_empty() {
        None
    } else {
        Some(excluded)
    };

    let args = SearchArgs {
        query: query.to_string(),
        options: Some(SearchOptions {
            num_results: max_results.max(1),
            region: "wt-wt".to_string(),
            safe_search: SafeSearchLevel::Moderate,
            time_range: None,
            backends: None,
            exclude_backends: exclude,
        }),
    };

    // A backstop, not the mechanism: the chain awaits every backend
    // before it aggregates, so one that hangs is paid for by every
    // search, and no partial answer exists to salvage. Excluding a
    // known-bad backend is the fix; this only stops the next one that
    // goes dark from holding a turn open for a minute.
    let response = match tokio::time::timeout(SEARCH_BUDGET, provider().search(&args)).await {
        Ok(r) => r.map_err(|e| WebSearchError::AllBackendsFailed(e.to_string()))?,
        Err(_) => {
            // Deliberately not recorded against any backend. The chain
            // returns one aggregate, so a timeout says nothing about
            // *which* member stalled — and the nine that answered in a
            // second would be blamed alongside the one that did not.
            // Resting is for the attributable case; a stall is for
            // `WEB_SEARCH_EXCLUDE_BACKENDS` and the log line below.
            tracing::warn!(
                "web search: backends did not answer within {:?} — run with \
                 RUST_LOG=daedra=debug and compare each backend's completion \
                 time, then add the stalled one to WEB_SEARCH_EXCLUDE_BACKENDS",
                SEARCH_BUDGET
            );
            return Err(WebSearchError::TimedOut);
        }
    };

    // Which backends' results survived the merge. NOT which answered:
    // daedra merges and truncates, so a backend that returned plenty can
    // be absent purely by ranking. Useful for tuning, unsound as a health
    // signal — see the note on DEFAULT_EXCLUDED.
    if tracing::enabled!(tracing::Level::DEBUG) {
        let surviving: HashSet<&str> = response
            .data
            .iter()
            .map(|r| r.metadata.source.as_str())
            .collect();
        tracing::debug!(
            ?surviving,
            results = response.data.len(),
            "web search: backends represented in the merged results"
        );
    }

    let results: Vec<SearchResult> = response
        .data
        .into_iter()
        .filter(|r| !r.url.trim().is_empty())
        .map(|r| SearchResult {
            title: if r.title.trim().is_empty() {
                r.url.clone()
            } else {
                r.title
            },
            url: r.url,
            content: r.description,
            fetched: false,
        })
        .take(max_results)
        .collect();
    if results.is_empty() {
        return Err(WebSearchError::Empty);
    }
    Ok(results)
}

/// Replace the snippets of the first `count` results with real article
/// text.
///
/// Every part of this is about the worst case, because the user is
/// staring at an empty bubble the whole time it runs — nothing streams
/// until it returns. A page that will time out costs the full
/// connect-plus-read budget, so fetching serially made the delay the
/// *sum* of the slow ones: four dead hosts were a measured 64 seconds.
/// Two things bound it instead. Fetches run [`FETCH_CONCURRENCY`] at a
/// time, so the cost is the slowest in a batch rather than the total.
/// And the whole phase is cut off at [`HYDRATE_BUDGET`] — whatever has
/// landed by then is used and the rest keep their snippets, which is
/// why the deadline is a `take_until` on the stream rather than a
/// `timeout` around it (a timeout would throw away the pages that did
/// arrive).
///
/// Concurrency stays low deliberately: a DOM costs several times its
/// page, and the container's `MemoryMax` is the real ceiling.
///
/// A failed fetch is not an error — that result simply keeps its
/// snippet, which is still better than dropping the source.
pub async fn hydrate(state: &AppState, results: &mut [SearchResult], count: usize) {
    let max_bytes = state.settings.web_search_max_page_bytes;
    let targets: Vec<(usize, String)> = results
        .iter()
        .enumerate()
        .take(count)
        .filter(|(_, r)| !is_opaque(&r.url))
        .map(|(i, r)| (i, r.url.clone()))
        .collect();
    if targets.is_empty() {
        return;
    }

    let fetches = futures_util::stream::iter(targets.into_iter().map(|(idx, url)| async move {
        let outcome = crate::safefetch::fetch_page(&url, max_bytes).await;
        (idx, url, outcome)
    }))
    .buffer_unordered(FETCH_CONCURRENCY);

    let deadline = tokio::time::sleep(HYDRATE_BUDGET);
    tokio::pin!(deadline);
    let mut landed = fetches.take_until(deadline);

    let mut done = 0usize;
    while let Some((idx, url, outcome)) = landed.next().await {
        done += 1;
        match outcome {
            Ok(page) => {
                let result = &mut results[idx];
                result.content = page.text;
                result.fetched = true;
                if !page.title.trim().is_empty() {
                    result.title = page.title;
                }
            }
            Err(e) => {
                tracing::debug!(url = %url, "web search: page fetch failed: {e}");
            }
        }
    }
    tracing::debug!(fetched = done, "web search: hydrate finished");
}

/// True for a URL that is a redirector rather than a document. Google
/// News RSS items are the case that matters: the link is an opaque
/// `CBMi…` blob that bounces to the publisher, so fetching it spends a
/// timeout to arrive somewhere with no article at it. The snippet the
/// feed already gave us is strictly better than that.
fn is_opaque(url: &str) -> bool {
    url.contains("news.google.com/rss/articles/")
}

/// Fetch one page by URL. Used when the user's turn already names the
/// page they want read ("summarise https://…").
pub async fn fetch(state: &AppState, page_url: &str) -> Result<SearchResult, WebSearchError> {
    if !state.settings.web_search_enabled {
        return Err(WebSearchError::Disabled);
    }
    let page =
        crate::safefetch::fetch_page(page_url, state.settings.web_search_max_page_bytes).await?;
    Ok(SearchResult {
        title: if page.title.trim().is_empty() {
            page.url.clone()
        } else {
            page.title
        },
        url: page.url,
        content: page.text,
        fetched: true,
    })
}

/// First `http(s)` URL in the text, if any — decides fetch-this-page vs
/// search-the-web. Strips the wrapping punctuation a URL picks up when
/// it's pasted mid-sentence or inside markdown.
pub fn extract_url(text: &str) -> Option<String> {
    for raw in text.split_whitespace() {
        let candidate = raw.trim_start_matches(['(', '<', '[', '"', '\'']);
        if !candidate.starts_with("http://") && !candidate.starts_with("https://") {
            continue;
        }
        let trimmed =
            candidate.trim_end_matches([')', '>', ']', '"', '\'', '.', ',', ';', ':', '!', '?']);
        // Require an actual host after the scheme — a bare "https://"
        // is punctuation, not a link.
        let Some(host_start) = trimmed.find("//").map(|i| i + 2) else {
            continue;
        };
        if trimmed.len() > host_start {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Fold results into the `system` message injected at the head of the
/// conversation. Markdown links so a model that cites inline produces
/// something the renderer can already turn into a real anchor.
pub fn build_context(results: &[SearchResult]) -> String {
    let mut prompt = String::from(
        "relevant results from a live web search — cite the source title and \
         link when you use one, and say plainly when the results don't answer \
         the question:\n\n",
    );
    for r in results {
        let body: String = r.content.chars().take(RESULT_CHAR_BUDGET).collect();
        prompt.push_str(&format!("[{}]({})\n{}\n\n", r.title, r.url, body.trim()));
    }
    prompt
}

/// Rewrite the user's turn into a standalone search query using prior
/// context, so "what about the second one?" doesn't get searched
/// verbatim. Non-streaming, one short call — same shape as
/// [`crate::ollama::summarize_title`].
pub async fn refine_query(
    state: Arc<AppState>,
    model: &str,
    history: &[ChatMessage],
    user_msg: &str,
) -> Result<String, reqwest::Error> {
    // The current turn is already the tail of `history`; drop it so it
    // isn't shown twice, then take the few turns before it.
    let prior = history
        .split_last()
        .map(|(_, rest)| rest)
        .unwrap_or(history);
    let mut transcript = String::new();
    for m in prior.iter().rev().take(QUERY_HISTORY_TURNS).rev() {
        let excerpt: String = m.content.chars().take(QUERY_HISTORY_CHARS).collect();
        transcript.push_str(&format!("{}: {}\n", m.role, excerpt));
    }

    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: "You turn a chat message into a web search query. Resolve \
                 pronouns and references using the earlier conversation. Reply \
                 with the query only — no quotes, no explanation, no prefix \
                 like 'query:'. Keep it under 15 words."
                .into(),
            images: None,
        },
        ChatMessage {
            role: "user".into(),
            content: format!(
                "earlier conversation:\n{transcript}\n\nlatest message: \
                 {user_msg}\n\nReply with only the search query."
            ),
            images: None,
        },
    ];

    let url = format!(
        "{}/api/chat",
        state.settings.ollama_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });

    let res = state
        .http_client
        .post(&url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;

    #[derive(serde::Deserialize)]
    struct Wrapper {
        message: ChatMessage,
    }
    let parsed: Wrapper = res.json().await?;
    Ok(sanitize_query(&parsed.message.content))
}

/// Keep the first line, drop wrapping quotes and a "query:" prefix, cap
/// the length. Returns an empty string when nothing usable survives —
/// the caller falls back to the raw user turn.
fn sanitize_query(raw: &str) -> String {
    let first = raw
        .trim()
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let mut s = first.trim().to_string();
    for prefix in ["query:", "search:", "search query:"] {
        if s.to_lowercase().starts_with(prefix) {
            s = s[prefix.len()..].trim().to_string();
        }
    }
    let s = s.trim_matches(['"', '\'', '`', '*']).trim();
    s.chars().take(MAX_QUERY_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(content: &str) -> SearchResult {
        SearchResult {
            title: "T".into(),
            url: "https://example.com".into(),
            content: content.to_string(),
            fetched: false,
        }
    }

    #[test]
    fn extract_url_finds_bare_link() {
        assert_eq!(
            extract_url("summarise https://example.com/post for me"),
            Some("https://example.com/post".into())
        );
    }

    #[test]
    fn extract_url_strips_wrapping_punctuation() {
        assert_eq!(
            extract_url("see (https://example.com/a)."),
            Some("https://example.com/a".into())
        );
        assert_eq!(
            extract_url("read <https://example.com/b>"),
            Some("https://example.com/b".into())
        );
        assert_eq!(
            extract_url("this one: https://example.com/c, then stop"),
            Some("https://example.com/c".into())
        );
    }

    #[test]
    fn extract_url_takes_the_first_of_several() {
        assert_eq!(
            extract_url("https://one.example https://two.example"),
            Some("https://one.example".into())
        );
    }

    #[test]
    fn extract_url_ignores_prose_and_bare_schemes() {
        assert_eq!(extract_url("what happened at the summit today"), None);
        assert_eq!(extract_url("the scheme https:// alone"), None);
        assert_eq!(extract_url("ftp://example.com/x"), None);
    }

    #[test]
    fn build_context_truncates_long_pages() {
        // 'z' appears nowhere in the header or the URL, so its count is
        // exactly the page text that survived truncation.
        let long = "z".repeat(RESULT_CHAR_BUDGET * 3);
        let out = build_context(&[result(&long)]);
        assert!(out.contains("[T](https://example.com)"));
        assert_eq!(out.matches('z').count(), RESULT_CHAR_BUDGET);
    }

    #[tokio::test]
    async fn search_is_refused_when_disabled() {
        // test_defaults leaves the feature off, which is also what a
        // fresh deploy looks like.
        let state = crate::create_test_state();
        assert!(!is_configured(&state));
        assert!(matches!(
            search(&state, "anything", 5).await,
            Err(WebSearchError::Disabled)
        ));
        assert!(matches!(
            fetch(&state, "https://example.com").await,
            Err(WebSearchError::Disabled)
        ));
    }

    #[tokio::test]
    async fn hydrate_keeps_the_snippet_when_a_fetch_fails() {
        let state = crate::create_test_state();
        // Loopback is refused by the fetch guard, so this exercises the
        // failure path without touching the network.
        let mut results = vec![SearchResult {
            title: "T".into(),
            url: "http://127.0.0.1/article".into(),
            content: "the original snippet".into(),
            fetched: false,
        }];
        hydrate(&state, &mut results, 1).await;
        assert_eq!(results[0].content, "the original snippet");
        assert!(!results[0].fetched);
    }

    #[test]
    fn sanitize_query_unwraps_model_chatter() {
        assert_eq!(sanitize_query("  \"rust async book\"  "), "rust async book");
        assert_eq!(sanitize_query("Query: rust async book"), "rust async book");
        assert_eq!(
            sanitize_query("rust async book\nsome trailing rambling"),
            "rust async book"
        );
        assert_eq!(sanitize_query("   "), "");
    }

    #[test]
    fn sanitize_query_caps_length() {
        let long = "word ".repeat(200);
        assert_eq!(sanitize_query(&long).chars().count(), MAX_QUERY_CHARS);
    }
}
