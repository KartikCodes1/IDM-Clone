import { describe, expect, it } from "vitest";

import { validateAddForm } from "./validate";

const SHA = "0123456789abcdef".repeat(4);

describe("validateAddForm", () => {
  it("accepts a trimmed http or https url and leaves optional fields out", () => {
    const result = validateAddForm({ url: "  https://example.com/a.zip  ", folder: "  ", sha256: "" });
    expect(result).toEqual({ ok: true, value: { url: "https://example.com/a.zip" } });
    expect(validateAddForm({ url: "http://example.com/a", folder: "", sha256: "" }).ok).toBe(true);
  });

  it("rejects empty and non-http urls with a friendly message", () => {
    for (const url of ["", "   ", "ftp://example.com/a", "file:///C:/a", "javascript:alert(1)", "example.com/a", "https://"]) {
      const result = validateAddForm({ url, folder: "", sha256: "" });
      expect(result.ok, url).toBe(false);
      if (!result.ok) expect(result.errors.url, url).toMatch(/http/i);
    }
  });

  it("rejects urls over 8192 bytes", () => {
    const url = `https://example.com/${"a".repeat(8200)}`;
    const result = validateAddForm({ url, folder: "", sha256: "" });
    expect(result.ok).toBe(false);
  });

  it("requires an absolute folder when one is given", () => {
    expect(validateAddForm({ url: "https://a.test/x", folder: String.raw`D:\Files`, sha256: "" }).ok).toBe(true);
    expect(validateAddForm({ url: "https://a.test/x", folder: String.raw`\\server\share\dir`, sha256: "" }).ok).toBe(true);
    for (const folder of ["Downloads", String.raw`..\x`, "/tmp", String.raw`C:relative`]) {
      const result = validateAddForm({ url: "https://a.test/x", folder, sha256: "" });
      expect(result.ok, folder).toBe(false);
      if (!result.ok) expect(result.errors.folder).toBeDefined();
    }
  });

  it("accepts exactly 64 hex characters for the checksum", () => {
    const ok = validateAddForm({ url: "https://a.test/x", folder: "", sha256: ` ${SHA.toUpperCase()} ` });
    expect(ok).toEqual({ ok: true, value: { url: "https://a.test/x", sha256: SHA.toUpperCase() } });
    for (const sha256 of [SHA.slice(1), `${SHA}0`, `${SHA.slice(1)}g`, "zz"]) {
      const result = validateAddForm({ url: "https://a.test/x", folder: "", sha256 });
      expect(result.ok, sha256).toBe(false);
      if (!result.ok) expect(result.errors.sha256).toMatch(/64/);
    }
  });

  it("reports every invalid field at once", () => {
    const result = validateAddForm({ url: "x", folder: "rel", sha256: "1" });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(Object.keys(result.errors).sort()).toEqual(["folder", "sha256", "url"]);
  });
});
