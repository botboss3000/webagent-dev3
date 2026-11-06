"""Start the web container: hand over to the Rust launcher, `webagent-v3 --launch`.

The launcher (crates/modules/worker/src/launch.rs) creates the protected directories and
sockets, then starts the promotion broker, the Rust and Python tool workers and the API
under restricted identities. The image command and broker inputs still name this file.
"""
import os

os.execv("/usr/local/bin/webagent-v3", ["/usr/local/bin/webagent-v3", "--launch"])
