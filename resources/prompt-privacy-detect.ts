import rules from "./prompt-privacy-rules.json";

const tokens = new RegExp(rules.tokens, "g");
// Selected provider formats and context-plus-entropy are adapted from
// Gitleaks v8.30.1 and CredSweeper v1.19.6 (see docs/licenses).
const providerTokens = new RegExp(rules.providerTokens, "g");
const webhook = new RegExp(rules.webhook, "g");
const genericKey = new RegExp(rules.genericKey, "gi");
const genericKeyExcludedName = new RegExp(rules.genericKeyExcludedName, "i");
const genericKeyPlainValue = new RegExp(rules.genericKeyPlainValue);
const email = new RegExp(rules.email, "g");
const keyMarker = new RegExp(rules.keyMarker, "g");
const keyBody = new RegExp(rules.keyBody);
const field = new RegExp(rules.field, "gi");
export const fieldName = new RegExp(rules.fieldName, "i");
const passwordName = new RegExp(rules.passwordName, "i");
const uriCredential = new RegExp(rules.uriCredential, "gi");
const authToken = new RegExp(rules.authToken, "gid");
const weakName = new RegExp(rules.weakName, "i");
const weakValue = new RegExp(rules.weakValue);
const fieldValue = new RegExp(rules.fieldValue);
const placeholder = new RegExp(rules.placeholder, "i");
const nearbyName = new RegExp(rules.nearbyName, "i");
const nonSecretValue = new RegExp(rules.nonSecretValue);

export function isFieldValue(text: string, name: string): boolean {
  const value = text.trim();
  return value.length >= (passwordName.test(name) ? 1 : 8) && !placeholder.test(value)
    && (!weakName.test(name) || weakValue.test(value));
}

function keyEntropy(value: string): number {
  const counts = new Uint16Array(128);
  for (let i = 0; i < value.length; i++) counts[value.charCodeAt(i)]++;
  let entropy = 0;
  for (const count of counts) {
    if (!count) continue;
    const p = count / value.length;
    entropy -= p * Math.log2(p);
  }
  return entropy;
}

type Hit = {
  start: number;
  end: number;
  spelling: string;
  original?: string;
  name?: string;
  strength: number;
  expandable: boolean;
};

// A value cut by a pattern's length limit is extended to the whole token, so
// the replacement never leaves the tail of a secret value the prompt. A single
// trailing padding equals sign belongs to a long value too.
const valueCharacter = /[A-Za-z0-9_+~-]/;

function expand(text: string, hit: Hit): Hit {
  if (!hit.expandable || hit.original !== undefined) return hit;
  let { start, end } = hit;
  while (start > 0 && valueCharacter.test(text[start - 1])) start -= 1;
  while (end < text.length && valueCharacter.test(text[end])) end += 1;
  if (end - start >= 20 && text[end] === "=" && text[end + 1] !== "=") end += 1;
  return { ...hit, start, end, spelling: text.slice(start, end) };
}

// A JWT-shaped candidate must still parse as a JWT header. The other formats
// are fixed by their prefix and length, so only this one needs decoding.
function structured(spelling: string): boolean {
  if (!spelling.startsWith("eyJ")) return true;
  const parts = spelling.split(".");
  if (parts.length !== 3) return false;
  try {
    const header = JSON.parse(Buffer.from(parts[0], "base64url").toString("utf8"));
    return typeof header === "object" && header !== null && typeof header.alg === "string";
  } catch {
    return false;
  }
}

function plainWord(spelling: string): boolean {
  return /^[a-z]+$/.test(spelling) && spelling.length < 24;
}

function valueShaped(spelling: string): boolean {
  if (spelling.length < 12) return false;
  const classes = [/[A-Z]/, /[a-z]/, /[0-9]/, /[^A-Za-z0-9]/]
    .filter(pattern => pattern.test(spelling)).length;
  return classes >= 2 && keyEntropy(spelling) >= 3.0;
}

// Score a candidate from the value alone and from the words just before it, so
// an identifier near "key" is judged differently from the same value in prose.
// Strong formats skip scoring; identifiers and timestamps are never secrets
// unless a password field names them.
function accepted(hit: Hit, text: string): boolean {
  if (!structured(hit.spelling)) return false;
  const password = hit.name !== undefined && passwordName.test(hit.name);
  if (!password && (nonSecretValue.test(hit.spelling) || plainWord(hit.spelling))) return false;
  if (hit.strength >= 3) return true;
  let score = hit.strength;
  if (hit.name !== undefined && nearbyName.test(hit.name)) score += 1;
  if (valueShaped(hit.spelling)) score += 1;
  const lineStart = text.lastIndexOf("\n", hit.start - 1) + 1;
  if (nearbyName.test(text.slice(Math.max(lineStart, hit.start - 48), hit.start))) score += 2;
  return score >= 3;
}

function collect(text: string): Hit[] {
  const hits: Hit[] = [];
  const add = (start: number, spelling: string, strength: number,
    options: { name?: string; original?: string; expandable?: boolean } = {}) => {
    hits.push({ start, end: start + spelling.length, spelling, strength, expandable: options.expandable ?? true, ...options });
  };
  for (const match of text.matchAll(tokens)) add(match.index!, match[0], 3);
  for (const match of text.matchAll(providerTokens)) add(match.index!, match[0], 3);
  for (const match of text.matchAll(webhook)) {
    add(match.index! + match[0].length - match[1].length, match[1], 3, { expandable: false });
  }
  for (const match of text.matchAll(genericKey)) {
    const value = match[2];
    if (genericKeyExcludedName.test(match[1]) || placeholder.test(value)
      || genericKeyPlainValue.test(value) || keyEntropy(value) < 3.1) continue;
    add(match.index! + match[0].length - value.length, value, 1, { name: match[1] });
  }
  for (const match of text.matchAll(authToken)) {
    const value = match[1] ?? match[2];
    if (!isFieldValue(value, "")) continue;
    const [start] = (match.indices![1] ?? match.indices![2])!;
    add(start, value, 2);
  }
  if (text.includes("@")) {
    const credentials: [number, number][] = [];
    for (const match of text.matchAll(uriCredential)) {
      const end = match.index! + match[0].length - 1;
      credentials.push([end - match[1].length, end]);
      if (isFieldValue(match[2], "password")) add(end - match[1].length, match[1], 3, { name: "password" });
    }
    let index = 0;
    for (const match of text.matchAll(email)) {
      const start = match.index!;
      while (credentials[index]?.[1] <= start) index++;
      const credential = credentials[index];
      if (!credential || credential[0] > start) add(start, match[0], 3, { expandable: false });
    }
  }
  if (text.includes("PRIVATE KEY")) {
    let opened: RegExpMatchArray | undefined;
    for (const match of text.matchAll(keyMarker)) {
      if (match[1] === "BEGIN") opened = match;
      else if (opened) {
        const body = text.slice(opened.index! + opened[0].length, match.index!);
        if (opened[2] === match[2] && body.length >= 32 && (keyBody.test(body) || match[2] === "PGP PRIVATE KEY BLOCK")) {
          add(opened.index!, text.slice(opened.index!, match.index! + match[0].length), 3, { expandable: false });
        }
        opened = undefined;
      }
    }
  }
  for (const match of text.matchAll(field)) {
    const matchValue = fieldValue.exec(text.slice(match.index! + match[0].length));
    const spelling = matchValue?.slice(1).find(candidate => candidate !== undefined);
    if (spelling === undefined) continue;
    const name = match.groups?.name ?? match.groups?.flag ?? "";
    const start = match.index! + match[0].length + matchValue![0].indexOf(spelling);
    // JSON can appear inside prompt text. Decode only a double-quoted JSON
    // member, not shell arguments or environment-file values.
    if (name && matchValue?.[1]?.includes("\\") && text[match.index! - 1] === '"'
      && match[0].slice(name.length).trim() === '":') {
      let original: string | undefined;
      try { original = JSON.parse(matchValue[0]); } catch { /* Keep the literal spelling. */ }
      if (original !== undefined) {
        if (isFieldValue(original, name)) add(start, spelling, 3, { name, original });
        continue;
      }
    }
    if (isFieldValue(spelling, name)) add(start, spelling, 3, { name: name || undefined });
  }
  return hits;
}

export function* discover(text: string): Iterable<{ spelling: string; original?: string }> {
  for (const raw of collect(text)) {
    const hit = expand(text, raw);
    if (!accepted(hit, text)) continue;
    yield hit.original === undefined ? { spelling: hit.spelling } : { spelling: hit.spelling, original: hit.original };
  }
}