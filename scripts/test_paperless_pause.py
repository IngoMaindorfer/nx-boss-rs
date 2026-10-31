#!/usr/bin/env python3
"""A missing Paperless secret must not take local scanning offline."""
import json
import urllib.request
import os
import sys
import tempfile
from pathlib import Path
from test_shutdown import free_port, request, start_server, stop_server

with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    (root / 'auth-pepper').write_bytes(os.urandom(32))
    (root / 'auth-pepper').chmod(0o600)
    (root / 'scans').mkdir()
    config = root / 'config.yaml'
    config.write_text(f'jobs:\n  Local:\n    output_path: {root / "scans"}\npaperless:\n  url: http://127.0.0.1:9\n  token_file: {root / "missing-token"}\n')
    with (root / 'server.log').open('wb') as log:
        scanner, admin = free_port(), free_port()
        process = start_server(str(Path(sys.argv[1]).resolve()), config, scanner, admin, log, 'none')
        try:
            status, body = request(scanner, 'POST', '/NmWebService/batch', b'{"job_id":0}')
            assert status == 200
            batch = json.loads(body)['batch_id']
            image = (Path(__file__).resolve().parent.parent / 'tests/fixtures/scan_page.jpg').read_bytes()
            multipart = (b'--test-boundary\r\nContent-Disposition: form-data; name="image"; filename="page.jpg"\r\nContent-Type: image/jpeg\r\n\r\n' + image
                + b'\r\n--test-boundary\r\nContent-Disposition: form-data; name="parameter"\r\n\r\n'
                + json.dumps({'batch_id': batch}).encode() + b'\r\n--test-boundary--\r\n')
            req = urllib.request.Request(f'http://127.0.0.1:{scanner}/NmWebService/image', data=multipart,
                headers={'Content-Type': 'multipart/form-data; boundary=test-boundary'}, method='POST')
            with urllib.request.urlopen(req, timeout=5) as response:
                assert response.status == 200
            assert request(scanner, 'PUT', '/NmWebService/batch/' + batch, b'{}')[0] == 200
            metadata = json.loads((root / 'scans' / batch / 'metadata.json').read_text())
            assert metadata['completed'] and metadata['paperless']['document_id'] is None
            assert (root / 'scans' / batch / 'page.jpg').read_bytes() == image
        finally:
            stop_server(process)
    assert 'paperless.config_unavailable' in (root / 'server.log').read_text()
print('Missing Paperless token: scanner healthy, local scan saved with pending delivery, clean shutdown')
