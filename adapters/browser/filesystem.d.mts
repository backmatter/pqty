export type PackageFileReader = {
  readonly size: number;
  readInto(target: Uint8Array, offset: number): number;
  text(): Promise<string>;
  close(): void;
};
/** Dedicated-worker OPFS adapter; reads directly into a WASM memory view. */
export function openPackageFile(
  url: string | URL,
  file: {digest: string; size: number},
  options?: {signal?: AbortSignal},
): Promise<PackageFileReader>;
