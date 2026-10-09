# score_directory_with_jev

Arguments: path, search_criteria, optional file_glob (basename pattern such as
*.js), recursive (default false), include_content (default false), and offset
(default 0). The worker discovers matching regular files without first
showing the working agent a directory listing. Recursive scans stop after
10,000 entries or eight levels; each call scores up to 32 matches and returns
next_offset for another page. Without file_glob, the original immediate
directory-name scoring remains available.

include_content sends the first 1,024 bytes of each matched file to TypeSafe
as an untrusted excerpt. This requires automatic file_read permission; all
modes require automatic file_list and Decision permission. The result returns
relative paths, scores from 0 to 4, confidence, evidence mode and truncation
information. Verify promising files with file_read before claiming what code
does. File excerpts may omit the relevant implementation.
