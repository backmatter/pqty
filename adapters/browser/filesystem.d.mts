export type PackageFileReader = {
  readonly size: number;
  readInto(target: Uint8Array, offset: number): number;
  /** Whole-file read into one string; for small text files. Prefer readInto. */
  text(): Promise<string>;
  close(): void;
};
/**
 * Dedicated-worker OPFS adapter; reads directly into a WASM memory view.
 * Readers are shared where read-only sync handles exist (Chromium), otherwise
 * exclusive until close().
 */
export function openPackageFile(
  url: string | URL,
  file: { digest: string; size: number },
  options?: { signal?: AbortSignal },
): Promise<PackageFileReader>;
