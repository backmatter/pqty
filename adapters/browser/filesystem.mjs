// Dedicated-worker adapter. OPFS owns persistence, atomic publication and file
// locking; its synchronous read writes directly into the caller's WASM view.
import { sha256 } from "@noble/hashes/sha2.js";

const limit = 128 * 1024 * 1024; // Existing pqty per-file trust bound.
const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");

async function consume(stream, file, write, signal) {
  const reader = stream.getReader();
  const hash = sha256.create();
  let size = 0;
  try {
    for (;;) {
      signal?.throwIfAborted();
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > file.size) throw new Error("Package file exceeds its declared size");
      hash.update(value);
      await write?.(value);
    }
    signal?.throwIfAborted();
    if (size !== file.size || `sha256:${hex(hash.digest())}` !== file.digest) {
      throw new Error("Package file integrity mismatch");
    }
  } finally {
    hash.destroy();
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

/**
 * Prepare one immutable file in browser-managed storage, then open a native
 * random-access reader. Requires a dedicated worker and OPFS. Fetch chooses
 * stream chunk sizes; no full-file JavaScript byte buffer is allocated.
 * @param {string | URL} input
 * @param {{digest: string, size: number}} file
 * @param {{signal?: AbortSignal}} [options]
 * @returns {Promise<{size: number, readInto(target: Uint8Array, offset: number): number, text(): Promise<string>, close(): void}>}
 */
export async function openPackageFile(input, file, { signal } = {}) {
  signal?.throwIfAborted();
  if (!/^sha256:[a-f0-9]{64}$/.test(file?.digest ?? "")
      || !Number.isSafeInteger(file?.size) || file.size < 0 || file.size > limit) {
    throw new Error("Invalid package file identity");
  }
  const url = new URL(input, globalThis.location?.href);
  if (url.username || url.password || url.search || url.hash
      || (url.protocol !== "https:" && !(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)))) {
    throw new Error("Package files require an immutable HTTPS URL");
  }
  const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle("pqty.package-files.v1", { create: true });
  // The native Web Lock serializes publication across tabs without a timer or
  // application lock file. Read-only OPFS handles remain shareable afterwards.
  return navigator.locks.request(`pqty:${file.digest}`, { signal }, async () => {
    const handle = await directory.getFileHandle(file.digest.slice(7), { create: true });
    let valid = false;
    const cached = await handle.getFile();
    if (cached.size === file.size) {
      try { await consume(cached.stream(), file, undefined, signal); valid = true; }
      catch { signal?.throwIfAborted(); }
    }
    if (!valid) {
      const response = await fetch(url, { signal, credentials: "omit", cache: "no-store" });
      if (!response.ok || !response.body) {
        await response.body?.cancel().catch(() => {});
        throw new Error(`Package file unavailable (${response.status})`);
      }
      const output = await handle.createWritable();
      try {
        await consume(response.body, file, chunk => output.write(chunk), signal);
        // createWritable publishes atomically at close, after verification.
        await output.close();
      } catch (error) {
        await output.abort().catch(() => {});
        throw error;
      }
    }
    signal?.throwIfAborted();
    const reader = await handle.createSyncAccessHandle({ mode: "read-only" });
    return {
      size: file.size,
      text() { return handle.getFile().then(file => file.text()); },
      readInto(target, offset) {
        if (!Number.isSafeInteger(offset) || offset < 0) throw new Error("Invalid package file offset");
        const length = Math.min(target.byteLength, Math.max(0, file.size - offset));
        return length ? reader.read(target.subarray(0, length), { at: offset }) : 0;
      },
      close() { reader.close(); },
    };
  });
}
