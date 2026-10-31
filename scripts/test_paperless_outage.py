"""Isolated proof: Paperless API outage does not end scans or scanner sessions."""
import hashlib, json, os, re, subprocess, sys, tempfile, threading, time, urllib.request, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
SOURCE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(SOURCE / 'scripts'))
from test_shutdown import free_port, request, start_server, stop_server, login
state = {'up': False, 'tasks': {}, 'successful_posts': 0}

class Paperless(BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def send_json(self, code, value):
        body = json.dumps(value).encode()
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        body = self.rfile.read(int(self.headers['Content-Length']))
        if not state['up']:
            return self.send_json(503, {'error': 'offline'})
        match = re.search(b'filename="([a-f0-9]{32}\\.pdf)"', body)
        if not match or b'%PDF-' not in body:
            return self.send_json(400, {'error': 'bad fixture'})
        state['successful_posts'] += 1
        filename = match[1].decode()
        task = str(uuid.UUID(bytes=hashlib.sha256(filename.encode()).digest()[:16]))
        state['tasks'][task] = {'task_id': task, 'status': 'SUCCESS', 'related_document_ids': [state['successful_posts']], 'input_data': {'filename': filename}}
        self.send_json(200, task)

    def do_GET(self):
        if not state['up']:
            return self.send_json(503, {'error': 'offline'})
        self.send_json(200, list(state['tasks'].values()))
api = ThreadingHTTPServer(('127.0.0.1', 0), Paperless)
threading.Thread(target=api.serve_forever, daemon=True).start()
binary = str(Path(sys.argv[1]).resolve())
started = time.monotonic()
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    (root / 'scans').mkdir()
    (root / 'auth-pepper').write_bytes(os.urandom(32))
    (root / 'auth-pepper').chmod(384)
    (root / 'scanner-password').write_text('shutdown-test-password')
    (root / 'scanner-password').chmod(384)
    (root / 'api-token').write_text('isolated-paperless-token')
    (root / 'api-token').chmod(384)
    config = root / 'config.yaml'
    config.write_text(f"jobs:\n  Outage:\n    output_path: {root / 'scans'}\npaperless:\n  url: http://127.0.0.1:{api.server_port}\n  token_file: {root / 'api-token'}\n")
    scanner, admin = (free_port(), free_port())
    with (root / 'server.log').open('wb') as log:
        process = start_server(binary, config, scanner, admin, log, 'userpass')
        try:
            token = login(scanner)

            def scan():
                before = time.monotonic()
                status, data = request(scanner, 'POST', '/NmWebService/batch', b'{"job_id":0}', token)
                assert status == 200
                batch = json.loads(data)['batch_id']
                image = (SOURCE / 'tests/fixtures/scan_page.jpg').read_bytes()
                body = b'--test-boundary\r\nContent-Disposition: form-data; name="image"; filename="page.jpg"\r\nContent-Type: image/jpeg\r\n\r\n' + image + b'\r\n--test-boundary\r\nContent-Disposition: form-data; name="parameter"\r\n\r\n' + json.dumps({'batch_id': batch}).encode() + b'\r\n--test-boundary--\r\n'
                req = urllib.request.Request(f'http://127.0.0.1:{scanner}/NmWebService/image', data=body, headers={'Content-Type': 'multipart/form-data; boundary=test-boundary', 'Authorization': 'Bearer ' + token, 'X-Forwarded-Proto': 'https'}, method='POST')
                with urllib.request.urlopen(req, timeout=5) as response:
                    assert response.status == 200
                assert request(scanner, 'PUT', '/NmWebService/batch/' + batch, b'{}', token)[0] == 200
                path = root / 'scans' / batch / 'metadata.json'
                meta = json.loads(path.read_text())
                assert meta['completed'] and meta['paperless']['document_id'] is None
                assert (path.parent / 'page.jpg').read_bytes() == image
                assert (path.parent / 'document.pdf').read_bytes().startswith(b'%PDF-')
                elapsed = time.monotonic() - before
                assert elapsed < 5
                print(json.dumps({'stage': 'local_scan_during_outage', 'seconds': round(elapsed, 3)}), flush=True)
                return path
            first = scan()
            for _ in range(450):
                if json.loads(first.read_text())['paperless']['upload_attempts'] >= 1:
                    break
                time.sleep(0.1)
            else:
                raise AssertionError('worker did not observe simulated outage')
            second = scan()
            state['up'] = True
            print(json.dumps({'stage': 'simulated_api_restored', 'elapsed_seconds': round(time.monotonic() - started, 1)}), flush=True)
            for _ in range(2400):
                if all((json.loads(p.read_text())['paperless']['document_id'] is not None for p in [first, second])):
                    break
                time.sleep(0.1)
            else:
                raise AssertionError('pending deliveries did not drain')
            assert request(scanner, 'GET', '/NmWebService/scansetting?job_id=0', None, token)[0] == 200
            assert len(state['tasks']) == 2 and state['successful_posts'] == 2
            print(json.dumps({'stage': 'both_deliveries_confirmed', 'same_scanner_session': True, 'documents': 2, 'successful_uploads': state['successful_posts'], 'elapsed_seconds': round(time.monotonic() - started, 1)}), flush=True)
        finally:
            stop_server(process)
api.shutdown()
