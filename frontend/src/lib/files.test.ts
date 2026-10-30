import { describe, expect, it } from "vitest";
import { isText, secretInputFromBytes, toBase64 } from "./files";

describe("secret files", () => {
  it("sends text files as text", () => {
    const bytes = new TextEncoder().encode("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n");
    expect(isText(bytes)).toBe(true);
    expect(secretInputFromBytes(bytes)).toEqual({ binary: false, value: "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n" });
  });

  it("sends binary files as exact base64", () => {
    const bytes = new Uint8Array([0, 159, 146, 150, 255]);
    expect(isText(bytes)).toBe(false);
    expect(secretInputFromBytes(bytes)).toEqual({ binary: true, value: { base64: "AJ+Slv8=" } });
    expect(isText(new Uint8Array([0xc3, 0x28]))).toBe(false);
    expect(toBase64(new Uint8Array(70000).fill(65)).length).toBe(93336);
  });
});
