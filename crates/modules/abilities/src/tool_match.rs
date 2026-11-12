//! Ranking rules for `list_tools` and `list_skills` queries. Scores come from the matcher's
//! `rank` operation (each candidate's closest text to the query); these rules add exact-name and
//! keyword evidence, pick the best match and keep a result list from ever being empty.
use crate::catalog::search_words;

// shortcut: starting values from the setting matcher's gates; tune on eval/tool-match-eval.jsonl.
/// At or above: a confident tool match, flagged `best_match` (tuned: wrong best 10% → 4% for
/// skills at 0.82, tools unchanged at 0.78).
pub const BEST: f32 = 0.78;
/// The best-match gate for skills, whose short texts score higher against loose queries.
pub const SKILL_BEST: f32 = 0.82;
/// Below: no real match; shown only when nothing clears it, flagged weak.
pub const WEAK: f32 = 0.65;
/// Added once per keyword found in the query, at most [`MAX_KEYWORD_HITS`] times.
pub const KEYWORD_BONUS: f32 = 0.05;
pub const MAX_KEYWORD_HITS: usize = 2;
/// Rows a query returns at most, and when nothing clears [`WEAK`].
pub const LIMIT: usize = 8;
pub const WEAK_ROWS: usize = 3;
// shortcut: preload gates start near BEST/SKILL_BEST; tune with the discovery trace.
/// At or above: a tool is loaded from the user's message before the first model call.
pub const PRELOAD_TOOL: f32 = 0.80;
/// Most tools and skills preloaded per message, and suggestions shown after them.
pub const PRELOAD_TOOLS: usize = 6;
pub const PRELOAD_SKILLS: usize = 2;
pub const SUGGESTIONS: usize = 5;
/// Message characters ranked, and parts scored on their own besides the whole message.
pub const PRELOAD_TEXT: usize = 2000;
pub const PRELOAD_CLAUSES: usize = 6;

/// One ranked candidate, by its index in the caller's list.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub index: usize,
    pub score: f32,
    pub best: bool,
    pub weak: bool,
}

/// The texts the matcher embeds for one candidate: its name in words, its description (first
/// sentence-length part) and its example requests.
pub fn texts(name: &str, description: &str, examples: &[String]) -> Vec<String> {
    let mut texts = vec![name.replace('_', " ")];
    let description: String = description.chars().take(300).collect();
    if !description.trim().is_empty() {
        texts.push(description);
    }
    texts.extend(examples.iter().take(8).cloned());
    texts
}

/// Whether the query names the candidate: its literal ID (`terminal_open`) or its words in order
/// and next to each other ("terminal open"). A one-word name counts only as the whole query, since
/// words like "shell" or "read" also appear in ordinary requests.
pub fn names(query: &str, name: &str) -> bool {
    let query_words = search_words(query);
    let name_words = search_words(name);
    if name_words.len() < 2 {
        return query_words == name_words;
    }
    query_words
        .windows(name_words.len())
        .any(|window| window == name_words.as_slice())
        || query
            .to_lowercase()
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| word == name.to_lowercase())
}

fn keyword_hits(query: &str, keywords: &[String]) -> usize {
    let words = search_words(query).join(" ");
    let padded = format!(" {words} ");
    keywords
        .iter()
        .filter(|keyword| {
            let keyword = search_words(keyword).join(" ");
            !keyword.is_empty() && padded.contains(&format!(" {keyword} "))
        })
        .count()
        .min(MAX_KEYWORD_HITS)
}

/// A candidate's score for `query`: 1.0 when the query names it, else the matcher score plus
/// keyword evidence, kept below a named match.
pub fn adjusted(query: &str, name: &str, keywords: &[String], score: f32) -> f32 {
    if names(query, name) {
        1.0
    } else {
        (score + KEYWORD_BONUS * keyword_hits(query, keywords) as f32).min(0.99)
    }
}

/// The parts of a user message scored on their own: the whole (capped) message first, then
/// its sentences and clauses, so one request among several still matches strongly.
pub fn clauses(text: &str) -> Vec<String> {
    let text: String = text.chars().take(PRELOAD_TEXT).collect();
    let mut parts = vec![text.trim().to_owned()];
    for sentence in text.split(['.', ';', '?', '!', '\n']) {
        for part in std::iter::once(sentence).chain(sentence.split(" and ")) {
            let part = part.trim();
            if part.split_whitespace().count() >= 3 && !parts.iter().any(|p| p == part) {
                parts.push(part.to_owned());
            }
        }
    }
    parts.retain(|part| !part.is_empty());
    parts.truncate(PRELOAD_CLAUSES + 1);
    parts
}

/// One preload candidate: `kind` is "tool" or "skill"; only `loadable` ones are picked.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub kind: &'static str,
    pub name: String,
    pub score: f32,
    pub loadable: bool,
}

/// Split candidates into preloads (tools at [`PRELOAD_TOOL`], skills at [`SKILL_BEST`], capped)
/// and the next [`SUGGESTIONS`] loadable ones at or above [`WEAK`], best first.
pub fn choose(mut candidates: Vec<Candidate>) -> (Vec<Candidate>, Vec<Candidate>) {
    candidates.retain(|c| c.loadable);
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.name.cmp(&b.name)));
    let (mut tools, mut skills) = (0, 0);
    let (mut preload, mut rest) = (Vec::new(), Vec::new());
    for candidate in candidates {
        let pick = match candidate.kind {
            "tool" if candidate.score >= PRELOAD_TOOL && tools < PRELOAD_TOOLS => {
                tools += 1;
                true
            }
            "skill" if candidate.score >= SKILL_BEST && skills < PRELOAD_SKILLS => {
                skills += 1;
                true
            }
            _ => false,
        };
        if pick {
            preload.push(candidate);
        } else if candidate.score >= WEAK && rest.len() < SUGGESTIONS {
            rest.push(candidate);
        }
    }
    (preload, rest)
}

/// Rank candidates `(name, keywords)` by `scores` (one per candidate). A named candidate always
/// comes first as a best match. Returns up to [`LIMIT`] rows at or above [`WEAK`], or else the
/// nearest [`WEAK_ROWS`] flagged weak, so a non-empty candidate list never yields no rows.
pub fn rank(
    query: &str,
    candidates: &[(&str, &[String])],
    scores: &[f32],
    best: f32,
) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = candidates
        .iter()
        .zip(scores)
        .enumerate()
        .map(|(index, ((name, keywords), score))| {
            let named = names(query, name);
            let score = adjusted(query, name, keywords, *score);
            Ranked {
                index,
                score,
                best: named || score >= best,
                weak: false,
            }
        })
        .collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.index.cmp(&b.index)));
    let strong = ranked.iter().take_while(|r| r.score >= WEAK).count();
    if strong > 0 {
        ranked.truncate(strong.min(LIMIT));
    } else {
        ranked.truncate(WEAK_ROWS);
        for row in &mut ranked {
            row.weak = true;
        }
    }
    ranked
}

/// Hints for compiled tools that have no `tool.json`: `(name, examples, keywords)`.
const COMPILED: &[(&str, &[&str], &[&str])] = &[
    (
        "file_list",
        &[
            "list the files in a folder",
            "what is in this directory",
            "show the folder contents",
        ],
        &["ls", "directory", "folder"],
    ),
    (
        "file_read",
        &[
            "read a source file",
            "open main.rs and show it",
            "look at the code in this file",
        ],
        &["read", "open", "view file"],
    ),
    (
        "file_search",
        &[
            "find where a function is defined",
            "search the code for a string",
            "grep for an error message",
        ],
        &["grep", "search", "find in files"],
    ),
    (
        "file_edit",
        &[
            "change one line in a file",
            "replace this text in the code",
            "fix a typo in a source file",
        ],
        &["edit", "replace", "patch"],
    ),
    (
        "file_write",
        &[
            "create a new file",
            "write this content to a file",
            "delete a file in the broker task",
        ],
        &["write", "create file", "save file"],
    ),
    (
        "state_inspect",
        &[
            "show this session's details",
            "list my recent runs",
            "inspect the current chat state",
            "read the session state",
        ],
        &["session", "runs", "state", "session state"],
    ),
    (
        "prompt_slot_read",
        &[
            "show the agent's instruction slots",
            "read the system prompt sections",
        ],
        &["prompt slots", "instructions"],
    ),
    (
        "prompt_slot_write",
        &[
            "add an instruction slot to the agent",
            "update the agent's prompt section",
        ],
        &["prompt slot", "instructions"],
    ),
    (
        "shell",
        &[
            "run a shell command",
            "run the tests in the terminal",
            "execute a bash script",
        ],
        &["bash", "terminal", "command line"],
    ),
    (
        "python",
        &[
            "run a python script",
            "calculate this with python code",
            "parse a csv file in python",
        ],
        &["python", "script"],
    ),
    (
        "checkout_edit",
        &[
            "edit a live checkout file directly",
            "change a source file without a broker task",
        ],
        &["checkout", "direct edit"],
    ),
    (
        "list_tools",
        &["find a tool for this task", "which tools can I load"],
        &["tools"],
    ),
    (
        "load_tool",
        &["load a tool by name", "enable the file_edit tool"],
        &["load tool"],
    ),
    (
        "list_skills",
        &["find guidance for this task", "which skills are there"],
        &["skills", "guidance"],
    ),
    (
        "load_skill",
        &[
            "load the ui proposals skill",
            "read the guidance for this ability",
        ],
        &["load skill"],
    ),
    (
        "context_assets",
        &[
            "unload a tool I no longer need",
            "list the tools and skills I loaded",
        ],
        &["unload", "loaded tools"],
    ),
    (
        "set_execution_mode",
        &[
            "switch to plan mode",
            "change to auto mode",
            "make this session read only",
        ],
        &[
            "mode",
            "plan mode",
            "auto mode",
            "read only mode",
            "read only",
        ],
    ),
    (
        "get_time",
        &["what time is it", "current time in UTC+2"],
        &["time", "clock"],
    ),
    (
        "get_date",
        &["what is today's date", "what day is it"],
        &["date", "today"],
    ),
    (
        "calculate",
        &[
            "add these two numbers",
            "what is 17 percent of 240",
            "multiply 12 by 34",
        ],
        &["calculate", "math", "arithmetic"],
    ),
    (
        "read_attachment",
        &[
            "read the file the user attached",
            "open the uploaded document",
        ],
        &["attachment", "uploaded file", "attached file", "attached"],
    ),
    (
        "compact_context",
        &[
            "summarise older messages to save context",
            "the conversation is too long, compact it",
        ],
        &["compact", "summarise context"],
    ),
    (
        "recall_compacted",
        &[
            "recall what was said before compaction",
            "read an earlier summarised part of this chat",
        ],
        &["recall", "compacted"],
    ),
    (
        "search_this_session",
        &[
            "find where we discussed the login bug",
            "search this conversation for a word",
        ],
        &["search conversation", "earlier message"],
    ),
    (
        "read_own_prompt",
        &["read my own prompt", "what are my instructions"],
        &["my prompt", "own prompt"],
    ),
    (
        "edit_own_prompt",
        &[
            "change my own instructions",
            "update a section of my prompt",
        ],
        &["edit prompt", "my instructions"],
    ),
    (
        "save_own_skill",
        &[
            "save this procedure as a skill",
            "write a reusable skill for myself",
        ],
        &["save skill", "own skill"],
    ),
    (
        "remove_own_skill",
        &["delete one of my own skills", "remove the skill I wrote"],
        &["remove skill", "delete skill"],
    ),
    (
        "register_user",
        &["help the user create an account", "open a sign-up form"],
        &["register", "sign up", "account"],
    ),
    (
        "register_webhook",
        &[
            "create a webhook for incoming events",
            "give me a URL to receive callbacks",
        ],
        &["webhook", "callback url"],
    ),
    (
        "list_webhooks",
        &["show my webhooks", "which webhook inboxes exist"],
        &["webhooks"],
    ),
    (
        "delete_webhook",
        &["remove a webhook", "revoke the webhook inbox"],
        &["delete webhook", "revoke webhook"],
    ),
    (
        "get_webhook_log",
        &["read events the webhook received", "show the webhook log"],
        &["webhook log", "webhook events"],
    ),
    (
        "update_task_checkpoint",
        &[
            "save progress on this task",
            "record a checkpoint of what is done",
        ],
        &["checkpoint", "progress"],
    ),
    (
        "get_task_checkpoint",
        &[
            "where did I leave off on this task",
            "read the saved task progress",
        ],
        &["checkpoint", "progress"],
    ),
    (
        "background_status",
        &[
            "is the background task finished",
            "check on the running background call",
        ],
        &["background task", "status"],
    ),
    (
        "background_cancel",
        &["stop the background task", "cancel the call still running"],
        &["cancel", "stop background"],
    ),
    (
        "present_plan",
        &[
            "show the user a plan with a checklist",
            "lay out the steps before starting",
        ],
        &["plan", "checklist"],
    ),
    (
        "present_chat_component",
        &[
            "show the user buttons to choose from",
            "ask the user to fill in a form",
            "show a choice panel",
        ],
        &["form", "buttons", "choice"],
    ),
    (
        "manage_agent_profiles",
        &[
            "save this agent identity as a preset",
            "apply a saved profile to an agent",
        ],
        &["profile", "identity preset"],
    ),
    (
        "rate_skill",
        &[
            "give feedback on a skill",
            "rate how useful this guidance was",
        ],
        &["rate", "feedback"],
    ),
    (
        "request_agent_login",
        &[
            "connect a ChatGPT subscription",
            "start a device login for a model provider",
        ],
        &["login", "subscription", "device code"],
    ),
    (
        "main_notes_read",
        &[
            "read the shared notes across sessions",
            "what decisions were recorded before",
        ],
        &["notes", "main notes"],
    ),
    (
        "main_notes_write",
        &[
            "record a decision for later sessions",
            "save a note about this goal",
        ],
        &["notes", "record decision"],
    ),
    (
        "script_save",
        &[
            "save these steps as a reusable script",
            "record a browser routine to replay",
        ],
        &["script", "save routine"],
    ),
    (
        "script_list",
        &["which scripts are saved", "list the session scripts"],
        &["scripts"],
    ),
    (
        "script_run",
        &["run the saved script", "replay the capture routine"],
        &["run script", "replay"],
    ),
    (
        "request_credential",
        &["ask the user for an API key", "request a password securely"],
        &["api key", "credential", "secret"],
    ),
    (
        "check_credential",
        &["is the API key saved", "check whether a credential exists"],
        &["credential", "api key"],
    ),
    (
        "list_vault_keys",
        &["which credentials are stored", "list the saved keys"],
        &["vault", "keys"],
    ),
    (
        "set_closer_status",
        &[
            "mark the turn complete",
            "say the task needs the user's input",
        ],
        &["complete", "needs input"],
    ),
    (
        "publish_main_message",
        &[
            "post a progress message to the main chat",
            "publish my reply",
        ],
        &["publish", "message"],
    ),
];

/// A tool's discovery hints: its `tool.json` `match` block, else the compiled table.
pub fn hints(name: &str) -> (Vec<String>, Vec<String>) {
    let owned = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    if let Some(found) = crate::tool_definitions::get(name).and_then(|d| d.match_data.as_ref()) {
        return (found.examples.clone(), found.keywords.clone());
    }
    COMPILED
        .iter()
        .find(|(tool, ..)| *tool == name)
        .map(|(_, examples, keywords)| (owned(examples), owned(keywords)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn named_tool_ranks_first_even_with_a_low_score() {
        let none = kw(&[]);
        let candidates = [("file_read", &none[..]), ("file_edit", &none[..])];
        let ranked = rank("use file_edit on main.rs", &candidates, &[0.9, 0.1], BEST);
        assert_eq!(ranked[0].index, 1);
        assert!(ranked[0].best && ranked[0].score == 1.0);
        let ranked = rank("file edit the header", &candidates, &[0.9, 0.1], BEST);
        assert_eq!(ranked[0].index, 1);
        // Name words elsewhere in the query, out of order, are not a name.
        let ranked = rank("edit the file header", &candidates, &[0.9, 0.1], BEST);
        assert_eq!(ranked[0].index, 0);
        assert!(!names("type into the open terminal", "terminal_open"));
        assert!(names("terminal open now", "terminal_open"));
        assert!(!names("close the shell", "shell"));
        assert!(names("shell", "shell"));
    }

    #[test]
    fn never_empty_and_weak_when_nothing_matches() {
        let none = kw(&[]);
        let candidates: Vec<(&str, &[String])> = ["a_tool", "b_tool", "c_tool", "d_tool"]
            .iter()
            .map(|n| (*n, &none[..]))
            .collect();
        let ranked = rank("bake bread", &candidates, &[0.2, 0.5, 0.4, 0.1], BEST);
        assert_eq!(ranked.len(), WEAK_ROWS);
        assert!(ranked.iter().all(|r| r.weak && !r.best));
        assert_eq!(ranked[0].index, 1);
    }

    #[test]
    fn thresholds_flag_best_and_cut_at_weak_and_limit() {
        let none = kw(&[]);
        let names: Vec<String> = (0..12).map(|i| format!("tool{i}")).collect();
        let candidates: Vec<(&str, &[String])> =
            names.iter().map(|n| (n.as_str(), &none[..])).collect();
        let mut scores = vec![0.7; 12];
        scores[3] = 0.9;
        scores[11] = 0.3;
        let ranked = rank("query", &candidates, &scores, BEST);
        assert_eq!(ranked.len(), LIMIT);
        assert_eq!(ranked[0].index, 3);
        assert!(ranked[0].best && !ranked[1].best && !ranked[0].weak);
    }

    #[test]
    fn keywords_add_evidence() {
        let shot = kw(&["screenshot"]);
        let none = kw(&[]);
        let candidates = [("page_text", &none[..]), ("capture", &shot[..])];
        let ranked = rank("take a screenshot", &candidates, &[0.70, 0.68], BEST);
        assert_eq!(ranked[0].index, 1);
    }

    #[test]
    fn clauses_split_sentences_and_long_and_joins() {
        let parts =
            clauses("Change the activity group. Show phone and desktop screenshots; let me choose");
        assert_eq!(
            parts[0],
            "Change the activity group. Show phone and desktop screenshots; let me choose"
        );
        assert!(parts.contains(&"Change the activity group".to_owned()));
        assert!(parts.contains(&"let me choose".to_owned()));
        // "phone and desktop" is too short to split into its own parts.
        assert!(parts.contains(&"Show phone and desktop screenshots".to_owned()));
        assert!(clauses("").is_empty());
        let long = "word ".repeat(PRELOAD_TEXT);
        assert!(clauses(&long)[0].chars().count() <= PRELOAD_TEXT);
        let many = "one two three. ".repeat(20).replace("three", "three x");
        assert!(clauses(&many).len() <= PRELOAD_CLAUSES + 1);
    }

    #[test]
    fn choose_applies_thresholds_caps_and_loadability() {
        let c = |kind: &'static str, name: &str, score: f32, loadable: bool| Candidate {
            kind,
            name: name.into(),
            score,
            loadable,
        };
        let mut candidates: Vec<_> = (0..8)
            .map(|i| c("tool", &format!("t{i}"), 0.95 - i as f32 * 0.01, true))
            .collect();
        candidates.push(c("tool", "loaded", 0.99, false));
        candidates.push(c("skill", "s1", 0.90, true));
        candidates.push(c("skill", "s2", 0.85, true));
        candidates.push(c("skill", "s3", 0.84, true));
        candidates.push(c("skill", "low", 0.81, true));
        candidates.push(c("tool", "weak", 0.70, true));
        candidates.push(c("tool", "none", 0.40, true));
        let (preload, suggest) = choose(candidates);
        let names = |v: &[Candidate]| v.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            preload.iter().filter(|c| c.kind == "tool").count(),
            PRELOAD_TOOLS
        );
        assert_eq!(
            preload.iter().filter(|c| c.kind == "skill").count(),
            PRELOAD_SKILLS
        );
        assert!(!names(&preload).contains(&"loaded".to_owned()));
        assert!(!names(&preload).contains(&"low".to_owned()));
        assert_eq!(suggest.len(), SUGGESTIONS);
        assert!(!names(&suggest).contains(&"none".to_owned()));
        assert!(suggest.windows(2).all(|w| w[0].score >= w[1].score));
    }
}
