# Search this session

## Usage

- Search this conversation's user and assistant messages by keywords; words match by stem (garden finds gardening), so add related words to the query (feline cat).
- Nearby matches merge into one excerpt

Search the public user and assistant messages of the current conversation. It does
not search other conversations (Agent Management's search_sessions does), private
model interactions, tool arguments or results.

- Words match by stem and common irregular forms: "garden" finds "gardening",
  "ran" finds "run". It does not know synonyms, so put related words in the same
  query ("feline cat kitten"); a message matching any of them counts, and one
  matching more or rarer words ranks higher. Common words like "the" are ignored.
- Matches close together in one message come back as one excerpt, with
  `matches` counting them. Excerpt length and result counts follow the search
  settings on the Agent Management ability page; `limit` can lower the count.
- Each result keeps the message's seq, ID, role, source run and timestamp. It
  reads up to 5000 messages; `scan_complete` is false when older ones were left out.

Excerpts can omit surrounding content and must not be treated as complete evidence.
Live Rust policy authorizes every read.
