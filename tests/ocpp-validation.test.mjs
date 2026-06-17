import assert from "node:assert/strict";
import { test } from "node:test";
import { withClient } from "./support/ocpp-test-utils.mjs";

test("responds with OccurenceConstraintViolation for missing required payload fields", { timeout: 5_000 }, async () => {
  await withClient(async ({ socket, nextClientMessage }) => {
    socket.send(JSON.stringify([2, "csms-1", "RemoteStartTransaction", {}]));

    const message = await nextClientMessage();
    assert.equal(message[0], 4);
    assert.equal(message[1], "csms-1");
    assert.equal(message[2], "OccurenceConstraintViolation");
  });
});

test("responds with PropertyConstraintViolation for enum violations", { timeout: 5_000 }, async () => {
  await withClient(async ({ socket, nextClientMessage }) => {
    socket.send(JSON.stringify([2, "csms-2", "Reset", { type: "Warm" }]));

    const message = await nextClientMessage();
    assert.equal(message[0], 4);
    assert.equal(message[1], "csms-2");
    assert.equal(message[2], "PropertyConstraintViolation");
  });
});

test("responds with FormationViolation for malformed CALL frames with message id", { timeout: 5_000 }, async () => {
  await withClient(async ({ socket, nextClientMessage }) => {
    socket.send(JSON.stringify([2, "csms-3", "Reset"]));

    const message = await nextClientMessage();
    assert.equal(message[0], 4);
    assert.equal(message[1], "csms-3");
    assert.equal(message[2], "FormationViolation");
  });
});

test("rejects pending calls when CALLRESULT violates response schema", { timeout: 5_000 }, async () => {
  await withClient(async ({ client, socket, nextServerMessage }) => {
    const promise = client.call("BootNotification", {
      chargePointVendor: "Kirby",
      chargePointModel: "TUI-16"
    });

    const call = await nextServerMessage();
    assert.equal(call[0], 2);
    assert.equal(call[2], "BootNotification");

    socket.send(JSON.stringify([3, call[1], { status: "Accepted" }]));

    await assert.rejects(promise, /OccurenceConstraintViolation/);
  });
});

test("rejects pending calls when the response times out", { timeout: 5_000 }, async () => {
  await withClient(async ({ client, nextServerMessage }) => {
    const promise = client.call(
      "BootNotification",
      {
        chargePointVendor: "Kirby",
        chargePointModel: "TUI-16"
      },
      20
    );

    const call = await nextServerMessage();
    assert.equal(call[0], 2);
    assert.equal(call[2], "BootNotification");

    await assert.rejects(promise, /BootNotification timed out after 20ms/);
  });
});

test("rejects pending calls when the socket closes", { timeout: 5_000 }, async () => {
  await withClient(async ({ client, socket, nextServerMessage }) => {
    const promise = client.call("Heartbeat", {});

    const call = await nextServerMessage();
    assert.equal(call[0], 2);
    assert.equal(call[2], "Heartbeat");

    socket.close();
    await assert.rejects(promise, /Connection closed/);
  });
});

test("preserves schema properties named id when stripping metadata", { timeout: 5_000 }, async () => {
  await withClient(async ({ client, socket, nextClientMessage }) => {
    const call = new Promise((resolve) => {
      client.once("call", (messageId, action, payload) => {
        client.reply(messageId, { status: "Unknown" });
        resolve({ action, payload });
      });
    });

    socket.send(JSON.stringify([2, "csms-clear-profile", "ClearChargingProfile", { id: 424242 }]));

    const received = await call;
    assert.deepEqual(received, { action: "ClearChargingProfile", payload: { id: 424242 } });

    const message = await nextClientMessage();
    assert.equal(message[0], 3);
    assert.equal(message[1], "csms-clear-profile");
    assert.equal(message[2].status, "Unknown");
  });
});
