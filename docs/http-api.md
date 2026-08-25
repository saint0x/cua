# Local HTTP API

CUA is driven only by the CLI and local HTTP API.

Default bind: `127.0.0.1:8765`.

Initial endpoints:

- `GET /`
- `GET /manifest`
- `GET /schemas`
- `GET /version`
- `GET /status`
- `GET /healthz`
- `POST /capture/screenshot`
- `GET /observe/desktop`
- `GET /observe/displays`
- `GET /observe/cursor`
- `POST /input/mouse`
- `POST /input/keyboard`
- `POST /input/clipboard`
- `POST /model/eval`
