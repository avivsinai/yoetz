#!/usr/bin/env node
import fs from 'node:fs/promises';
import { execSync } from 'node:child_process';
import { createRequire } from 'node:module';
import path from 'node:path';

const require = createRequire(import.meta.url);

// Prefer the bundled dependency shipped with releases/Homebrew. Fall back to a
// global install for source checkouts or older local setups.
// The package is ESM-only, so fallbacks use dynamic import or deep CJS require.
let getCookies, toCookieHeader;
try {
  ({ getCookies, toCookieHeader } = normalizeCookieModule(await import('@steipete/sweet-cookie')));
} catch {
  try {
    const globalRoot = execSync('npm root -g', { encoding: 'utf8' }).trim();
    ({ getCookies, toCookieHeader } = normalizeCookieModule(
      await import(path.join(globalRoot, '@steipete/sweet-cookie', 'dist', 'index.js'))
    ));
  } catch {
    const globalRoot = execSync('npm root -g', { encoding: 'utf8' }).trim();
    ({ getCookies, toCookieHeader } = normalizeCookieModule(
      require(path.join(globalRoot, '@steipete/sweet-cookie', 'dist', 'index.js'))
    ));
  }
}

const args = process.argv.slice(2);
const options = new Map();
for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (!arg.startsWith('--')) continue;
  const key = arg.slice(2);
  const value = args[i + 1] && !args[i + 1].startsWith('--') ? args[++i] : 'true';
  options.set(key, value);
}

const url = options.get('url') || 'https://chatgpt.com/';
const browsers = (options.get('browsers') || 'chrome,edge,firefox,safari')
  .split(',')
  .map((b) => b.trim())
  .filter(Boolean);
const names = options.get('names')
  ? options
      .get('names')
      .split(',')
      .map((n) => n.trim())
      .filter(Boolean)
  : undefined;

const timeoutMs = Number(options.get('timeout-ms')) || 30_000;

const { cookies, warnings } = await getCookies({
  url,
  browsers,
  names,
  timeoutMs,
});

const storageState = {
  cookies: cookies
    .map(toPlaywrightCookie)
    .filter(Boolean),
  origins: [],
};

const payload = {
  cookies,
  warnings,
  cookieHeader: toCookieHeader(cookies, { dedupeByName: true }),
  storageState,
};

const outputPath = options.get('output');
if (outputPath) {
  await fs.writeFile(outputPath, JSON.stringify(storageState, null, 2));
}

process.stdout.write(JSON.stringify(payload, null, 2));

function normalizeCookieModule(mod) {
  const resolved = mod?.default && !mod.getCookies ? mod.default : mod;
  if (typeof resolved?.getCookies !== 'function' || typeof resolved?.toCookieHeader !== 'function') {
    throw new Error('invalid @steipete/sweet-cookie module export shape');
  }
  return resolved;
}

function toPlaywrightCookie(cookie) {
  if (!cookie || !cookie.name || cookie.value === undefined) return null;
  const domain = cookie.domain || cookie.host || cookie.hostname;
  if (!domain) return null;

  let expires = cookie.expires ?? cookie.expiry ?? cookie.expirationDate ?? -1;
  if (typeof expires === 'string') {
    const num = Number(expires);
    expires = Number.isFinite(num) ? num : -1;
  }
  if (typeof expires === 'number' && expires > 1e12) {
    // Likely milliseconds
    expires = Math.floor(expires / 1000);
  }
  if (!Number.isFinite(expires)) {
    expires = -1;
  }

  return {
    name: cookie.name,
    value: String(cookie.value),
    domain,
    path: cookie.path || '/',
    expires,
    httpOnly: Boolean(cookie.httpOnly),
    secure: Boolean(cookie.secure),
    sameSite: normalizeSameSite(cookie.sameSite),
  };
}

function normalizeSameSite(value) {
  if (!value) return 'Lax';
  const v = String(value).toLowerCase();
  if (v.includes('strict')) return 'Strict';
  if (v.includes('none') || v.includes('no_restriction')) return 'None';
  return 'Lax';
}
