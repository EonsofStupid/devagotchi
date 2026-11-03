# The hub's site, when you keep it here

`compose.yaml` mounts the directory `DEVAGOTCHI_SITE_DIR` names (in `.env`) into the hub,
read-only. With it unset, this empty directory stands in and the hub serves its built-in
site. To serve your own, put a site here (start from a copy of `crates/hub/site/default/`)
and set `DEVAGOTCHI_SITE_DIR=./site`, or name any other directory, its files readable by all
(the hub runs as an unprivileged user); `install.sh --site DIR`
does it for a server. What a site holds: `crates/hub/README.md`, "The site".

The hub follows the directory: edit a file and it is served within seconds once it checks
out. `docker compose up -d hub` applies a change of `DEVAGOTCHI_SITE_DIR` itself.
