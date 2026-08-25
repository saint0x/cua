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
- `GET /capture/stream.mjpeg`: continuous MJPEG stream, newest-frame-per-tick, no backlog
- `GET /capture/stream.ws`: continuous WebSocket stream with JSON frame envelopes and binary JPEG frames
- `GET /observe/desktop`
- `GET /observe/displays`
- `GET /observe/cursor`
- `POST /input/mouse`
- `POST /input/keyboard`
- `POST /input/clipboard`
- `POST /model/eval`

`GET /status` reports `active_streams`; stream clients increment the count on connect and decrement after disconnect cleanup.
