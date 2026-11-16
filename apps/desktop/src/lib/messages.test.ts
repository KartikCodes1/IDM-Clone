import { describe, expect, it } from "vitest";

import { failureText } from "./actions";
import { formatRetryTime, retryAtOf, stateMessage } from "./messages";

const NOW = new Date(2026, 10, 16, 12, 0, 0).getTime();
const TIME_FORMAT = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });

describe("formatRetryTime", () => {
  it("shows_only_the_local_time_for_a_retry_later_today", () => {
    const at = new Date(2026, 10, 16, 14, 5, 0).getTime();
    expect(formatRetryTime(at, NOW)).toBe(TIME_FORMAT.format(at));
    expect(formatRetryTime(at, NOW)).toMatch(/05/);
  });

  it("adds_the_date_when_the_retry_is_on_another_day", () => {
    const at = new Date(2026, 10, 17, 0, 30, 0).getTime();
    const text = formatRetryTime(at, NOW);
    expect(text).toContain(TIME_FORMAT.format(at));
    expect(text.length).toBeGreaterThan(TIME_FORMAT.format(at).length);
  });

  it("uses_the_local_time_zone_not_utc", () => {
    const at = new Date(2026, 10, 16, 23, 59, 0).getTime();
    expect(formatRetryTime(at, NOW)).toBe(TIME_FORMAT.format(new Date(at)));
  });
});

describe("stateMessage", () => {
  const at = new Date(2026, 10, 16, 14, 5, 0).getTime();

  it("retry_waiting_says_when_the_retry_happens", () => {
    expect(stateMessage({ state: "retry_waiting" }, at, NOW)).toBe(`Retrying at ${formatRetryTime(at, NOW)}`);
  });

  it("retry_waiting_without_a_time_does_not_invent_one", () => {
    expect(stateMessage({ state: "retry_waiting" }, undefined, NOW)).toBe("Retrying soon");
  });

  it("retry_waiting_with_a_time_in_the_past_says_now", () => {
    expect(stateMessage({ state: "retry_waiting" }, NOW - 5000, NOW)).toBe("Retrying now");
  });

  it("a_retry_time_beyond_the_date_range_is_ignored_instead_of_throwing", () => {
    const bad = 8.7e15;
    const error = { code: "rate_limited", message: "m", retryable: true, details: { retry_at_ms: bad } };
    expect(retryAtOf({ last_error: error })).toBeUndefined();
    expect(stateMessage({ state: "retry_waiting" }, bad, NOW)).toBe("Retrying soon");
  });

  it("needs_url_refresh_and_needs_credentials", () => {
    expect(stateMessage({ state: "needs_url_refresh" })).toBe("Link expired: add the download again with a fresh URL");
    expect(stateMessage({ state: "needs_credentials" })).toBe("This server needs a login (not supported yet)");
  });

  it("paused_explains_disk_full_and_destination_unavailable_only", () => {
    expect(stateMessage({ state: "paused", state_reason: "disk_full" })).toMatch(/disk is full/i);
    expect(stateMessage({ state: "paused", state_reason: "destination_unavailable" })).toMatch(/folder or drive is unavailable/i);
    expect(stateMessage({ state: "paused", state_reason: "user" })).toBeUndefined();
    expect(stateMessage({ state: "paused" })).toBeUndefined();
  });

  it("interrupted_will_resume_automatically", () => {
    expect(stateMessage({ state: "interrupted", state_reason: "agent_shutdown" })).toBe("Will resume automatically");
  });

  it("failed_maps_the_code_and_never_shows_the_engine_text", () => {
    const message = stateMessage({ state: "failed", last_error: { code: "path_escape", message: "C:\\secret", retryable: false } });
    expect(message).toBe(failureText({ code: "path_escape" }));
    expect(message).not.toContain("secret");
  });

  it("other_states_have_no_extra_message", () => {
    for (const state of ["queued", "downloading", "completed", "cancelled", "finalizing", "teleporting"]) {
      expect(stateMessage({ state }), state).toBeUndefined();
    }
  });
});

describe("retryAtOf", () => {
  it("reads_the_retry_time_a_rate_limit_error_carries", () => {
    const error = { code: "rate_limited", message: "m", retryable: true, details: { retry_at_ms: 1234 } };
    expect(retryAtOf({ last_error: error })).toBe(1234);
    expect(retryAtOf({ last_error: { code: "x", message: "m", retryable: true } })).toBeUndefined();
    expect(retryAtOf({})).toBeUndefined();
  });
});

describe("failureText for the newer codes", () => {
  it("maps_every_download_error_code_to_readable_text", () => {
    const codes = [
      "network_timeout", "network_connect", "network_reset", "body_incomplete", "dns_failure", "rate_limited",
      "http_status", "auth_required", "url_expired", "tls_failure", "too_many_redirects", "redirect_loop",
      "redirect_downgrade", "redirect_blocked", "range_protocol_violation", "unsupported_content_encoding",
      "remote_changed", "size_mismatch", "checksum_mismatch", "disk_full", "destination_unavailable", "disk_io",
      "path_too_long", "file_system_limit", "secrets_unavailable", "blocked_by_policy", "blocked_by_antivirus",
      "internal", "path_escape", "finalize_locked", "retry_budget_exhausted", "part_file_invalid",
    ];
    const generic = failureText({ code: "no_such_code" });
    for (const code of codes) {
      const text = failureText({ code });
      expect(text, code).not.toBe(generic);
      expect(text, code).toMatch(/\S/);
    }
    expect(generic).toBe("The download failed.");
  });
});
