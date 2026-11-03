// @vitest-environment happy-dom
// Signing in from the link the hub logs on its first start (`/#code=<code>`).

import { flushPromises } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import LoginView from "../src/components/shell/LoginView.vue";
import { mountInApp } from "./mount";

const login = vi.hoisted(() => vi.fn(async () => {}));
vi.mock("../src/store", async (orig) => ({ ...(await orig<typeof import("../src/store")>()), login }));

describe("LoginView", () => {
  it("signs in with the code in the link and drops it from the address", async () => {
    history.replaceState(null, "", "/#code=K7QM2XPA");
    await mountInApp(LoginView, {});
    await flushPromises();
    expect(login).toHaveBeenCalledWith("K7QM2XPA", expect.any(String));
    expect(location.hash).toBe("");
  });
});
