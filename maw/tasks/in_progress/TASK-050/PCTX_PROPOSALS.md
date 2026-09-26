# PCTX proposals (TASK-050)

- 2026-09-26 (code-reviewer): reqwest is built with `default-features = false` and without `system-proxy`, so every HTTP client in cctg (hub Bot API, agent release download) honours only env proxies (`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`), never the Windows/macOS system proxy settings (`cargo tree -e features -i hyper-util`: no `client-proxy-system`). Worth a risk-lesson line in the hub/channel domains so later tasks do not promise "system proxy".
