"""Loopback Chromium -> real signal server -> Rust remote transport.

Build mobile/dist and the fintwind-remote direct_e2e example first; install the
signal service's locked dependencies. Run on Windows with Python websockets
15+, Chromium and OpenSSL. No system trust store or existing profile is used.
"""

import argparse
import asyncio
import base64
from contextlib import ExitStack
import hashlib
from http import HTTPStatus
import json
import logging
from mimetypes import guess_type
import os
from pathlib import Path
import runpy
import shutil
import socket
import ssl
import struct
import subprocess
import time
from urllib.request import ProxyHandler, build_opener
from uuid import uuid4

from websockets.asyncio.client import connect
from websockets.asyncio.server import serve
from websockets.datastructures import Headers
from websockets.exceptions import ConnectionClosed
from websockets.http11 import Response


ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / 'mobile/dist'
OWNED_PROCESS = runpy.run_path(str(ROOT / 'scripts/goal-opencode-e2e.py'))['OwnedProcess']
HTTP = build_opener(ProxyHandler({}))


class Stun(asyncio.DatagramProtocol):
    def connection_made(self, transport):
        self.transport = transport

    def datagram_received(self, data, address):
        if len(data) < 20 or data[:2] != b'\x00\x01' or data[4:8] != b'\x21\x12\xa4\x42':
            return
        ip = int.from_bytes(socket.inet_aton(address[0]), 'big') ^ 0x2112A442
        attr = struct.pack('!HHBBHI', 0x0020, 8, 0, 1, address[1] ^ 0x2112, ip)
        self.transport.sendto(struct.pack('!HH', 0x0101, len(attr)) + data[4:20] + attr, address)


class SignalProxy:
    def __init__(self, port, report):
        self.port = port
        self.report = report
        self.hold_answer = False
        self.held = None

    @staticmethod
    def serve_page(connection, request):
        path = request.path.split('?', 1)[0]
        if path == '/signal/v2':
            return None
        file = (DIST / (path.lstrip('/') or 'index.html')).resolve()
        if not file.is_relative_to(DIST) or not file.is_file():
            return connection.respond(HTTPStatus.NOT_FOUND, 'Not found')
        body = file.read_bytes()
        mime = {'.js': 'text/javascript', '.css': 'text/css'}.get(file.suffix, guess_type(file)[0])
        return Response(200, 'OK', Headers({'Content-Type': mime or 'application/octet-stream',
                                             'Content-Length': str(len(body))}), body)

    async def forward(self, browser):
        async with connect(f'ws://127.0.0.1:{self.port}/signal/v2', proxy=None, close_timeout=1) as upstream:
            role = None

            async def from_client():
                nonlocal role
                async for message in browser:
                    frame = json.loads(message)
                    if frame['type'] == 'join':
                        role = frame['role']
                        self.report[f'{role}Joins'] += 1
                    if role == 'phone' and frame['type'] == 'signal':
                        self.report['phoneOffers'] += 1
                    if frame['type'] == 'signal' and role == 'desktop' and self.hold_answer:
                        self.hold_answer = False
                        self.held = (upstream, message)
                        continue
                    await upstream.send(message)

            async def from_server():
                async for message in upstream:
                    frame = json.loads(message)
                    if role == 'desktop' and frame.get('sender') == 'phone':
                        self.report['forwardedOffers'] += 1
                    if role == 'phone' and frame['type'] == 'error':
                        self.report['phoneSignalErrors'].append(frame['message'])
                    await browser.send(message)

            tasks = [asyncio.create_task(from_client()), asyncio.create_task(from_server())]
            try:
                await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
                if upstream.close_code not in (None, 1005, 1006):
                    await browser.close(upstream.close_code, upstream.close_reason)


class Browser:
    def __init__(self, session):
        self.session = session
        self.sequence = 0

    async def call(self, method, params=None, session_id=None):
        self.sequence += 1
        message = {'id': self.sequence, 'method': method, 'params': params or {}}
        if session_id:
            message['sessionId'] = session_id
        await self.session.send(json.dumps(message))
        async with asyncio.timeout(8):
            while True:
                reply = json.loads(await self.session.recv())
                if reply.get('id') == self.sequence:
                    if 'error' in reply:
                        raise RuntimeError(reply['error'])
                    return reply['result']

    async def page(self, url):
        target = (await self.call('Target.createTarget', {'url': 'about:blank'}))['targetId']
        session = (await self.call('Target.attachToTarget', {'targetId': target, 'flatten': True}))['sessionId']
        await self.call('Page.enable', session_id=session)
        await self.call('Page.addScriptToEvaluateOnNewDocument', {'source': '''
          (() => {
            let hidden = false;
            Object.defineProperty(document, 'hidden', { get: () => hidden });
            window.setRemoteHidden = value => {
                hidden = value;
                document.dispatchEvent(new Event('visibilitychange'));
            };
            const Peer = window.RTCPeerConnection;
            window.RTCPeerConnection = class extends Peer {
                constructor(config) {
                    super({...config, iceServers: []});
                    window.lastRemotePeer = this;
                }
                createDataChannel(...args) {
                    const channel = super.createDataChannel(...args);
                    channel.addEventListener('message', event => {
                        if (channel === window.muteIncomingChannel) event.stopImmediatePropagation();
                    });
                    return window.lastRemoteChannel = channel;
                }
            };
            const send = RTCDataChannel.prototype.send;
            RTCDataChannel.prototype.send = function (...args) {
                if (this !== window.silentChannel) return send.apply(this, args);
            };
          })();
        '''}, session)
        await self.call('Page.navigate', {'url': url}, session)
        return target, session

    async def evaluate(self, session, source):
        result = await self.call('Runtime.evaluate', {'expression': source, 'returnByValue': True}, session)
        if 'exceptionDetails' in result:
            error = result['exceptionDetails']
            raise RuntimeError(error.get('exception', {}).get('description', error['text']))
        return result['result'].get('value')


async def until(label, read, seconds=8):
    async with asyncio.timeout(seconds):
        try:
            while not (value := await read()):
                await asyncio.sleep(0.04)
            return value
        except asyncio.CancelledError:
            print(f'Waiting for: {label}', flush=True)
            raise


def probe_health(port):
    try:
        with HTTP.open(f'http://127.0.0.1:{port}/healthz', timeout=0.3) as response:
            return json.load(response).get('service') == 'fintwind-signal'
    except OSError:
        return False


async def exercise(args, run, env, tls, pinned_key, report, stack):
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        signal_port = reservation.getsockname()[1]
    signal = OWNED_PROCESS(shutil.which('node'), [str(args.signal.resolve())],
                           run, dict(env, HOST='127.0.0.1', PORT=str(signal_port)))
    stack.callback(signal.close)
    await until('the local signaling process', lambda: asyncio.to_thread(probe_health, signal_port))
    assert signal.alive()

    stun, _ = await asyncio.get_running_loop().create_datagram_endpoint(Stun, local_addr=('127.0.0.1', 0))
    stack.callback(stun.close)
    stun_port = stun.get_extra_info('sockname')[1]
    proxy = SignalProxy(signal_port, report)
    async with serve(proxy.forward, '127.0.0.1', 0, ssl=tls, process_request=proxy.serve_page,
                     close_timeout=1, logger=logging.getLogger('remote-e2e.front')) as front:
        origin = f'https://127.0.0.1:{front.sockets[0].getsockname()[1]}'
        pairing_file = run / 'pairing'
        desktop = OWNED_PROCESS(str(args.desktop.resolve()), ['--relay-url', origin, '--profile-dir', str(run / 'desktop'),
                                '--pairing-file', str(pairing_file), '--out-dir', str(run), '--stun-port', str(stun_port),
                                '--timeout', str(args.timeout + 5)], run, env)
        stack.callback(desktop.close)
        profile = run / 'chromium'
        chrome = OWNED_PROCESS(str(args.browser.resolve()), [f'--user-data-dir={profile}', '--headless', '--no-first-run',
                               '--remote-debugging-port=0', '--no-proxy-server', '--disable-background-networking',
                               '--disable-features=WebRtcHideLocalIpsWithMdns', '--allow-loopback-in-peer-connection',
                               f'--ignore-certificate-errors-spki-list={pinned_key}', 'about:blank'], run, env)
        stack.callback(chrome.close)
        port_file = profile / 'DevToolsActivePort'

        async def started():
            if not desktop.alive():
                raise RuntimeError('Desktop fixture exited during startup; see status.jsonl')
            if not chrome.alive():
                raise RuntimeError('Chromium exited during startup')
            return pairing_file.is_file() and port_file.is_file() and report['desktopJoins'] > 0

        await until('fixture registration and Chromium debug port', started, 15)
        port, path = port_file.read_text().splitlines()
        async with connect(f'ws://127.0.0.1:{port}{path}', proxy=None, close_timeout=1) as session:
            browser = Browser(session)

            async def filled(session_id):
                return await browser.evaluate(session_id, 'document.body?.innerText.includes("E2E 会话")')

            for index in range(10):
                start = time.monotonic()
                url = pairing_file.read_text().strip() if index == 0 else origin
                offers = report['forwardedOffers']
                target, page = await browser.page(url)
                try:
                    await until(f'session list on open #{index + 1}', lambda: filled(page), 5)
                except TimeoutError:
                    report['pageOnTimeout'] = await browser.evaluate(page,
                        "({text: document.body?.innerText, peer: window.lastRemotePeer?.connectionState, ice: window.lastRemotePeer?.iceGatheringState, fixture: typeof window.setRemoteHidden})")
                    raise
                assert report['forwardedOffers'] == offers + 1
                report['openMs'].append(round((time.monotonic() - start) * 1000))
                if index < 9:
                    await browser.call('Target.closeTarget', {'targetId': target})

            def requests(kind):
                log = run / 'requests.jsonl'
                if not log.is_file():
                    return 0
                return log.read_text(encoding='utf-8').count(f'"type":"{kind}"')

            session_reads = requests('listSessions')
            await browser.evaluate(page, 'setRemoteHidden(true); setRemoteHidden(false)')

            async def refreshed():
                return requests('listSessions') > session_reads and await filled(page)

            await until('healthy foreground read', refreshed)
            assert report['forwardedOffers'] == 10
            report['healthyResumeReused'] = True
            start = time.monotonic()
            await browser.evaluate(page, 'setRemoteHidden(true); window.silentChannel = window.lastRemoteChannel; setRemoteHidden(false); setRemoteHidden(false)')
            await until('one reconnection of the silent channel', lambda: browser.evaluate(page,
                        "window.lastRemoteChannel !== window.silentChannel && window.lastRemoteChannel?.readyState === 'open' && document.body.innerText.includes('E2E 会话')"))
            assert report['forwardedOffers'] == 11
            report['silentResumeMs'] = round((time.monotonic() - start) * 1000)
            await browser.call('Target.closeTarget', {'targetId': target})

            proxy.hold_answer = True
            target, page = await browser.page(origin)

            async def held():
                return proxy.held

            old_socket, old_answer = await until('answer in flight before closing the page', held)
            await browser.call('Target.closeTarget', {'targetId': target})
            target, page = await browser.page(origin)

            async def replacement():
                return report['forwardedOffers'] >= 13

            await until('replacement offer on the same desktop socket', replacement)
            await old_socket.send(old_answer)
            await until('session list after late answer on the replaced attempt', lambda: filled(page))
            await browser.call('Target.closeTarget', {'targetId': target})
            target, page = await browser.page(origin)
            await until('next page after the delayed answer', lambda: filled(page))
            report['lateAnswerIgnored'] = True

            # Lose the acknowledgement after delivering a real write to Rust.
            # The next page must read the result, not send the prompt again.
            await browser.evaluate(page, "document.querySelector('.session-row').click()")
            await until('editable conversation', lambda: browser.evaluate(page, "!!document.querySelector('textarea:not(:disabled)')"))
            await browser.evaluate(page, "document.querySelector('textarea:not(:disabled)').focus()")
            await browser.call('Input.insertText', {'text': 'e2e-reconnect-once'}, page)
            await browser.evaluate(page, '''
              window.muteIncomingChannel = window.lastRemoteChannel;
              document.activeElement.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', ctrlKey: true, bubbles: true}));
            ''')

            async def submitted():
                return requests('sendPrompt') == 1

            await until('one prompt delivered to the desktop', submitted)
            await browser.call('Target.closeTarget', {'targetId': target})
            target, page = await browser.page(origin)
            await until('persisted reply after reopen', lambda: browser.evaluate(page,
                        "document.body?.innerText.includes('fixture-reply:e2e-reconnect-once')"))
            assert requests('sendPrompt') == 1
            report['promptSentOnce'] = True
            await browser.call('Target.closeTarget', {'targetId': target})
            assert report['desktopJoins'] == 1, report
            assert not report['phoneSignalErrors'], report

            # A different room proves the burst remains bounded and supplies
            # backoff metadata without any live desktop or credentials.
            async with connect(f'ws://127.0.0.1:{signal_port}/signal/v2', proxy=None, close_timeout=1) as quota:
                await quota.send(json.dumps({'type': 'join', 'version': 2, 'role': 'phone',
                                            'routeHash': hashlib.sha256(uuid4().bytes).hexdigest()}))
                assert json.loads(await quota.recv())['type'] == 'joined'
                for attempt in range(17):
                    encoded = base64.urlsafe_b64encode(attempt.to_bytes(32, 'big')).decode().rstrip('=')
                    await quota.send(json.dumps({'type': 'signal', 'version': 2, 'attempt': encoded,
                                                'seq': 0, 'data': 'AA'}))
                async with asyncio.timeout(3):
                    limited = json.loads(await quota.recv())
                assert limited['type'] == 'error' and 0 < limited['retryAfterMs'] <= 15_000
                report['quotaRetryAfterMs'] = limited['retryAfterMs']
            (run / 'finish').touch()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--browser', type=Path, required=True)
    parser.add_argument('--openssl', type=Path, required=True)
    parser.add_argument('--desktop', type=Path, default=ROOT / 'target/debug/examples/direct_e2e.exe')
    parser.add_argument('--signal', type=Path, default=ROOT / 'deploy/remote-control/signal/index.mjs')
    parser.add_argument('--timeout', type=int, default=65)
    args = parser.parse_args()
    run = ROOT / 'target/remote-reconnect-e2e' / uuid4().hex
    run.mkdir(parents=True)
    print(run, flush=True)
    env = {k: v for k, v in os.environ.items() if k.upper() in {'PATH', 'SYSTEMROOT', 'WINDIR', 'SYSTEMDRIVE', 'COMSPEC'}}
    for name in ('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP'):
        directory = run / name.lower()
        directory.mkdir()
        env[name] = str(directory)
    key, cert, ca = run / 'loopback.key', run / 'loopback.pem', run / 'ca.pem'
    extensions = run / 'server.ext'
    extensions.write_text('basicConstraints=critical,CA:FALSE\nsubjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n')

    def openssl(*arguments):
        return subprocess.run([str(args.openssl), *arguments], env=env, cwd=run,
                              capture_output=True, check=True, timeout=10).stdout

    openssl('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=Remote E2E CA',
            '-keyout', 'ca.key', '-out', 'ca.pem')
    openssl('req', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=127.0.0.1',
            '-keyout', 'loopback.key', '-out', 'server.csr')
    openssl('x509', '-req', '-in', 'server.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key', '-CAcreateserial',
            '-days', '1', '-extfile', 'server.ext', '-out', 'loopback.pem')
    public_key = subprocess.check_output([str(args.openssl), 'x509', '-in', str(cert), '-pubkey', '-noout'],
                                         env=env, cwd=run, timeout=5)
    der = subprocess.run([str(args.openssl), 'pkey', '-pubin', '-outform', 'DER'], env=env, cwd=run,
                         input=public_key, capture_output=True, check=True, timeout=5).stdout
    pinned = base64.b64encode(hashlib.sha256(der).digest()).decode()
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    env['SSL_CERT_FILE'] = str(ca)
    class BrowserCloseFilter(logging.Filter):
        def filter(self, record):
            return not (record.exc_info and isinstance(record.exc_info[1], ConnectionClosed))

    logging.getLogger('remote-e2e.front').addFilter(BrowserCloseFilter())
    report = {'desktopJoins': 0, 'phoneJoins': 0, 'phoneOffers': 0, 'forwardedOffers': 0,
              'phoneSignalErrors': [], 'openMs': [], 'passed': False}
    async def run_case():
        with ExitStack() as stack:
            await asyncio.wait_for(exercise(args, run, env, tls, pinned, report, stack), args.timeout)

    try:
        asyncio.run(run_case(), loop_factory=asyncio.SelectorEventLoop)
        report['passed'] = True
    finally:
        (run / 'pairing').unlink(missing_ok=True)
        (run / 'report.json').write_text(json.dumps(report, indent=2))
        print(json.dumps(report), flush=True)


if __name__ == '__main__':
    main()
