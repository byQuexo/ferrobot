import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { StreamableHTTPError } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

import { isSessionLost } from "../src/mcp-client.ts";

describe("mcp client", () => {
  it("treats a 404 as a lost session (daemon restarted)", () => {
    assert.equal(isSessionLost(new StreamableHTTPError(404, "Session not found")), true);
    assert.equal(isSessionLost(new StreamableHTTPError(401, "Unauthorized")), false);
    assert.equal(isSessionLost(new Error("boom")), false);
  });
});
