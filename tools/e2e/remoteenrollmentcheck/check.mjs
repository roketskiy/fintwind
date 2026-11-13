import { createServer } from 'node:http';
import { createChallenge, consumeChallenge, mintAdmissionToken } from '../../../deploy/remote-control/relay/admission.mjs';
import { spawn } from 'node:child_process';

const binary = process.argv[2];
if (!binary) throw new Error('checker path required');
const challenges = new Map();
const secret = 'local-fixture-only-secret-do-not-use-in-production';
const server = createServer(async (request, response) => {
  let body = '';
  for await (const chunk of request) body += chunk;
  const input = JSON.parse(body);
  if (request.url === '/v1/admission/challenge') {
    response.end(JSON.stringify(createChallenge(challenges, input.publicKey, 'loopback')));
    return;
  }
  if (request.url === '/v1/admission/token') {
    const result = consumeChallenge(challenges, input, 'loopback');
    if (result.error) {
      response.writeHead(403).end();
      return;
    }
    response.end(JSON.stringify(mintAdmissionToken(secret, result.installationId)));
    return;
  }
  response.writeHead(404).end();
});
await new Promise((resolve, reject) => server.once('error', reject).listen(0, '127.0.0.1', resolve));
try {
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('listener unavailable');
  const env = {};
  for (const name of ['SystemRoot', 'WINDIR', 'TEMP', 'TMP']) {
    if (process.env[name]) env[name] = process.env[name];
  }
  const child = spawn(binary, [`http://127.0.0.1:${address.port}`], { env, stdio: 'inherit' });
  const result = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', resolve);
  });
  if (result !== 0) throw new Error(`checker exited ${result}`);
} finally {
  await new Promise(resolve => server.close(resolve));
}
