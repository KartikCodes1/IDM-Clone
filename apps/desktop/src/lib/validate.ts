// Client-side checks for the Add download form. They only spare the user a round trip:
// the engine validates everything again and stays the authority.

export interface AddFormInput {
  url: string;
  folder: string;
  sha256: string;
}

export interface AddFormValue {
  url: string;
  folder?: string;
  sha256?: string;
}

export interface AddFormErrors {
  url?: string;
  folder?: string;
  sha256?: string;
}

export type AddFormResult = { ok: true; value: AddFormValue } | { ok: false; errors: AddFormErrors };

const MAX_URL_BYTES = 8192;
const SHA256_PATTERN = /^[0-9a-fA-F]{64}$/;
const ABSOLUTE_FOLDER = /^(?:[A-Za-z]:[\\/]|\\\\[^\\/]+[\\/])/;
const URL_MESSAGE = "Enter an http or https link.";

function urlError(url: string): string | undefined {
  if (url === "") return URL_MESSAGE;
  if (new TextEncoder().encode(url).length > MAX_URL_BYTES) return "That link is too long.";
  try {
    const parsed = new URL(url);
    const web = parsed.protocol === "http:" || parsed.protocol === "https:";
    return web && parsed.hostname !== "" ? undefined : URL_MESSAGE;
  } catch {
    return URL_MESSAGE;
  }
}

export function validateAddForm(input: AddFormInput): AddFormResult {
  const url = input.url.trim();
  const folder = input.folder.trim();
  const sha256 = input.sha256.trim();
  const errors: AddFormErrors = {};

  const badUrl = urlError(url);
  if (badUrl !== undefined) errors.url = badUrl;
  if (folder !== "" && !ABSOLUTE_FOLDER.test(folder)) {
    errors.folder = String.raw`Enter a full folder path such as C:\Downloads, or leave this empty.`;
  }
  if (sha256 !== "" && !SHA256_PATTERN.test(sha256)) {
    errors.sha256 = "A SHA-256 checksum is 64 hexadecimal characters.";
  }
  if (Object.keys(errors).length > 0) return { ok: false, errors };

  const value: AddFormValue = { url };
  if (folder !== "") value.folder = folder;
  if (sha256 !== "") value.sha256 = sha256;
  return { ok: true, value };
}
