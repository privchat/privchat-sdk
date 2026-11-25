#!/usr/bin/env python3
"""Loopback receiver for the real Alertmanager gate."""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

output = Path(sys.argv[2])


class Receiver(BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            size = int(self.headers.get("Content-Length", "0"))
            if self.path != "/alert" or not 0 < size <= 65536:
                raise ValueError("unexpected webhook request")
            payload = json.loads(self.rfile.read(size))
            alerts = payload["alerts"]
            matching = [
                alert for alert in alerts
                if alert["status"] == "firing"
                and alert["labels"]["alertname"] == "PrivChatTimelineDeliveryDead"
                and alert["labels"]["job"] == "privchat-acceptance"
            ]
            if matching:
                tmp = output.with_suffix(".tmp")
                tmp.write_text(json.dumps(payload), encoding="utf-8")
                tmp.replace(output)
        except (TypeError, ValueError, KeyError):
            self.send_error(400)
            return
        self.send_response(204)
        self.end_headers()


with HTTPServer(("127.0.0.1", int(sys.argv[1])), Receiver) as server:
    output.with_suffix(".ready").touch()
    server.serve_forever()
