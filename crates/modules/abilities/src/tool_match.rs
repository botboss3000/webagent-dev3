//! Ranking rules for `list_tools` and `list_skills` queries. Scores come from the matcher's
//! `rank` operation (each candidate's closest text to the query); these rules add exact-name and
//! keyword evidence, pick the best match and keep a result list from ever being empty.
use crate::catalog::search_words;

// shortcut: starting values from the setting matcher's gates; tune on eval/tool-match-eval.jsonl.
/// At or above: a confident match, flagged `best_match`.
pub const BEST: f32 = 0.78;
/// Below: no real match; shown only when nothing clears it, flagged weak.
pub const WEAK: f32 = 0.65;
/// Added once per keyword found in the query, at most [`MAX_KEYWORD_HITS`] times.
pub const KEYWORD_BONUS: f32 = 0.05;
pub const MAX_KEYWORD_HITS: usize = 2;
/// Rows a query returns at most, and when nothing clears [`WEAK`].
pub const LIMIT: usize = 8;
pub const WEAK_ROWS: usize = 3;

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

/// Whether the query names the candidate: its exact ID, or every word of it.
pub fn names(query: &str, name: &str) -> bool {
    let query_words = search_words(query);
    let name_words = search_words(name);
    query.to_lowercase().contains(&name.to_lowercase())
        || (!name_words.is_empty() && name_words.iter().all(|w| query_words.contains(w)))
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

/// Rank candidates `(name, keywords)` by `scores` (one per candidate). A named candidate always
/// comes first as a best match. Returns up to [`LIMIT`] rows at or above [`WEAK`], or else the
/// nearest [`WEAK_ROWS`] flagged weak, so a non-empty candidate list never yields no rows.
pub fn rank(query: &str, candidates: &[(&str, &[String])], scores: &[f32]) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = candidates
        .iter()
        .zip(scores)
        .enumerate()
        .map(|(index, ((name, keywords), score))| {
            let named = names(query, name);
            let score = if named {
                1.0
            } else {
                (score + KEYWORD_BONUS * keyword_hits(query, keywords) as f32).min(0.99)
            };
            Ranked {
                index,
                score,
                best: named || score >= BEST,
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
        ],
        &["session", "runs", "state"],
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
        &["mode", "plan mode", "auto mode"],
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
        &["attachment", "uploaded file"],
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
        let ranked = rank("use file_edit on main.rs", &candidates, &[0.9, 0.1]);
        assert_eq!(ranked[0].index, 1);
        assert!(ranked[0].best && ranked[0].score == 1.0);
        let ranked = rank("file edit the header", &candidates, &[0.9, 0.1]);
        assert_eq!(ranked[0].index, 1);
    }

    #[test]
    fn never_empty_and_weak_when_nothing_matches() {
        let none = kw(&[]);
        let candidates: Vec<(&str, &[String])> = ["a_tool", "b_tool", "c_tool", "d_tool"]
            .iter()
            .map(|n| (*n, &none[..]))
            .collect();
        let ranked = rank("bake bread", &candidates, &[0.2, 0.5, 0.4, 0.1]);
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
        let ranked = rank("query", &candidates, &scores);
        assert_eq!(ranked.len(), LIMIT);
        assert_eq!(ranked[0].index, 3);
        assert!(ranked[0].best && !ranked[1].best && !ranked[0].weak);
    }

    #[test]
    fn keywords_add_evidence() {
        let shot = kw(&["screenshot"]);
        let none = kw(&[]);
        let candidates = [("page_text", &none[..]), ("capture", &shot[..])];
        let ranked = rank("take a screenshot", &candidates, &[0.70, 0.68]);
        assert_eq!(ranked[0].index, 1);
    }
}
