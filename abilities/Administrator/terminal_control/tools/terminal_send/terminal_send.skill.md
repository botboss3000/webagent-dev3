# terminal_send

Requires the exact `sequence` from a fresh `terminal_read` or `terminal_wait`. New output or an earlier send invalidates that observation; read again after a stale-observation error. Do not replay input after an uncertain result.
