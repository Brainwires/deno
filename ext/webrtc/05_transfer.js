// Copyright 2018-2026 the Deno authors. MIT license.

// Transferable RTCDataChannel (W3C WebRTC 6.2, "Transfering data channel";
// webrtc-extensions exposes it to every worker). A channel posted in a
// transfer list moves to another realm while its connection stays where it
// was made: the transfer steps detach the original (readyState "closed",
// [[IsTransferable]] false) and entangle a MessageChannel between it and the
// received channel. The port carries, in order, the data holder, then the
// connection's events for the channel one way and the received channel's
// send() / close() / bufferedAmountLowThreshold the other.
//
// The steps are registered for every realm by 98_global_scope_shared.js, so
// a worker that has not touched WebRTC yet receives a channel too.

/// <reference path="../../core/internal.d.ts" />

(function () {
const { core, primordials } = __bootstrap;
const {
  op_message_port_recv_message_sync,
  op_webrtc_dc_close,
  op_webrtc_dc_send,
  op_webrtc_dc_set_low_threshold,
} = core.ops;
const {
  ObjectPrototypeIsPrototypeOf,
  String,
  TypeError,
} = primordials;

const { DOMException } = core.loadExtScript(
  "ext:deno_web/01_dom_exception.js",
);
const { Event } = core.loadExtScript("ext:deno_web/02_event.js");
const {
  deserializeJsMessageData,
  getMessagePortId,
  MessageChannel,
} = core.loadExtScript("ext:deno_web/13_message_port.js");

let dc;
function internals() {
  return dc ??= core.loadExtScript("ext:deno_webrtc/01_webrtc.js")
    .dataChannelInternals;
}

// --- The realm that has the connection ---------------------------------------

// The RTCDataChannel transfer steps, given value; returns the resource that
// carries the data holder (a MessagePort's).
function transferSteps(channel) {
  const {
    _init,
    _pc,
    _state,
    _transferPort,
    RTCDataChannel,
  } = internals();
  if (
    !ObjectPrototypeIsPrototypeOf(RTCDataChannel.prototype, channel) ||
    channel[_pc] === null
  ) {
    throw new DOMException(
      "This RTCDataChannel cannot be transferred",
      "DataCloneError",
    );
  }
  const state = channel[_state];
  // Step 1: not after the task that created the channel, after send(), or
  // a second time.
  if (!state.transferable) {
    throw new DOMException(
      "RTCDataChannel is not transferable: it was created in an earlier task, sent data or was transferred already",
      "DataCloneError",
    );
  }
  const init = channel[_init];
  const pc = channel[_pc];
  const sctp = pc[_state].sctp;
  // Steps 2 to 10: the data holder. [[BufferedAmount]] is 0 (nothing was
  // sent); the priority (webrtc-priority) and the transport's
  // maxMessageSize (what send() checks) go with it.
  const holder = {
    readyState: state.readyState,
    label: init.label,
    ordered: init.ordered,
    maxPacketLifeTime: init.maxPacketLifeTime ?? null,
    maxRetransmits: init.maxRetransmits ?? null,
    protocol: init.protocol,
    negotiated: init.negotiated,
    id: channel.id,
    priority: init.priority ?? "low",
    maxMessageSize: sctp === null || sctp === undefined
      ? null
      : sctp.maxMessageSize,
  };
  const { port1, port2 } = new MessageChannel();
  port1.postMessage(holder);
  // Steps 11 and 12.
  state.transferable = false;
  state.readyState = "closed";
  channel[_transferPort] = port1;
  port1.onmessage = (event) => onProxyRequest(channel, event.data);
  // The connection keeps the process alive while it is connected; the port
  // to a channel in another realm adds nothing to that.
  port1.unref();
  return core.getTransferableResource("MessagePort").send(port2);
}

// A request of the received channel, run on the connection.
function onProxyRequest(channel, message) {
  const { _channel, _pc, _rid, _state } = internals();
  const pc = channel[_pc];
  if (pc[_state].closed || message === null || typeof message !== "object") {
    return;
  }
  switch (message.t) {
    case "send":
      try {
        op_webrtc_dc_send(
          pc[_rid],
          channel[_channel],
          message.text,
          message.bytes,
        );
      } catch (error) {
        // What send() accepted could not go out: the received channel
        // reports it as a browser does (error, then close).
        postToProxy(channel, {
          t: "senderror",
          message: error?.message ?? String(error),
        });
      }
      break;
    case "close":
    case "threshold":
      // A channel the engine has let go of already: nothing to do.
      try {
        if (message.t === "close") {
          op_webrtc_dc_close(pc[_rid], channel[_channel]);
        } else {
          op_webrtc_dc_set_low_threshold(
            pc[_rid],
            channel[_channel],
            message.value,
          );
        }
      } catch {
        // The closing or closed channel's events still reach the proxy.
      }
      break;
  }
}

function postToProxy(channel, message) {
  const { _transferPort } = internals();
  const port = channel[_transferPort];
  if (getMessagePortId(port) === null) return;
  port.postMessage(message);
}

// An engine event of a transferred channel (01_webrtc.js'
// dispatchEngineEvent), handed to the realm the channel went to.
function forwardEvent(channel, event) {
  const { _pc, _state, _transferPort } = internals();
  switch (event.type) {
    case "dcopen": {
      const sctp = channel[_pc][_state].sctp;
      postToProxy(channel, {
        t: "open",
        id: event.id ?? null,
        maxMessageSize: sctp === null || sctp === undefined
          ? null
          : sctp.maxMessageSize,
      });
      break;
    }
    case "dcclosing":
      postToProxy(channel, { t: "closing" });
      break;
    case "dcclose": {
      postToProxy(channel, { t: "close" });
      const port = channel[_transferPort];
      if (getMessagePortId(port) !== null) port.close();
      break;
    }
    case "dcerror":
      postToProxy(channel, {
        t: "error",
        errorDetail: event.errorDetail,
        detail: event.message,
        sctpCauseCode: event.sctpCauseCode ?? null,
        label: event.label ?? null,
        id: event.id ?? null,
      });
      break;
    case "dcsent":
      postToProxy(channel, { t: "sent", bytes: event.bytes });
      break;
    case "dcmessage":
      postToProxy(channel, {
        t: "message",
        text: event.text,
        data: event.data,
      });
      break;
  }
}

// --- The realm the channel went to -------------------------------------------

// The RTCDataChannel transfer-receiving steps, given the resource that
// transferSteps returned.
function transferReceivingSteps(rid) {
  const { _proxyPort, illegalConstructorKey, RTCDataChannel } = internals();
  const port = core.getTransferableResource("MessagePort").receive(rid);
  // The data holder was posted before the port was transferred, so it is
  // queued on the port already.
  const raw = op_message_port_recv_message_sync(getMessagePortId(port));
  if (raw === null) {
    port.close();
    throw new DOMException(
      "The transferred RTCDataChannel has no data holder",
      "DataCloneError",
    );
  }
  const holder = deserializeJsMessageData(raw)[0];
  const channel = new RTCDataChannel(illegalConstructorKey, null, {
    channel: null,
    label: holder.label,
    ordered: holder.ordered,
    maxPacketLifeTime: holder.maxPacketLifeTime,
    maxRetransmits: holder.maxRetransmits,
    protocol: holder.protocol,
    negotiated: holder.negotiated,
    id: holder.id,
    priority: holder.priority,
    readyState: holder.readyState,
    maxMessageSize: holder.maxMessageSize,
  });
  channel[_proxyPort] = port;
  if (holder.readyState === "closed") {
    // A transport that is closed already: the channel is closed, with no
    // event ("announce the channel as closed" aborts on a closed channel).
    port.close();
  } else {
    port.onmessage = (event) => onOwnerEvent(channel, event.data);
  }
  return channel;
}

// An event of the connection for a received channel.
function onOwnerEvent(channel, message) {
  const {
    _proxyPort,
    _state,
    RTCError,
    RTCErrorEvent,
    channelSent,
    deliverChannelMessage,
    failChannel,
    fire,
  } = internals();
  if (message === null || typeof message !== "object") return;
  const state = channel[_state];
  switch (message.t) {
    case "open":
      if (message.id !== null && message.id !== undefined) {
        state.id = message.id;
      }
      if (message.maxMessageSize !== null) {
        state.maxMessageSize = message.maxMessageSize;
      }
      // W3C "announce the channel as open".
      if (state.readyState === "closing" || state.readyState === "closed") {
        break;
      }
      state.readyState = "open";
      fire(channel, new Event("open"));
      break;
    case "message":
      deliverChannelMessage(channel, message.text, message.data);
      break;
    case "sent":
      channelSent(channel, message.bytes);
      break;
    case "closing":
      if (state.readyState === "closed") break;
      state.readyState = "closing";
      fire(channel, new Event("closing"));
      break;
    case "close": {
      const port = channel[_proxyPort];
      if (getMessagePortId(port) !== null) port.close();
      if (state.readyState === "closed") break;
      state.readyState = "closed";
      fire(channel, new Event("close"));
      break;
    }
    case "error": {
      const detail = message.errorDetail === "data-channel-failure"
        ? "data-channel-failure"
        : "sctp-failure";
      const cause = message.sctpCauseCode === null ||
          message.sctpCauseCode === undefined
        ? ""
        : ` (SCTP cause code ${message.sctpCauseCode})`;
      const reason = message.detail ? `: ${message.detail}` : "";
      fire(
        channel,
        new RTCErrorEvent("error", {
          error: new RTCError(
            {
              errorDetail: detail,
              sctpCauseCode: detail === "sctp-failure"
                ? message.sctpCauseCode ?? null
                : null,
            },
            `The data channel '${message.label ?? channel.label}' (id ${
              message.id ?? channel.id ?? "unassigned"
            }) failed${reason}${cause}`,
          ),
        }),
      );
      break;
    }
    case "senderror":
      failChannel(channel, new TypeError(String(message.message)));
      break;
  }
}

function postToOwner(channel, message) {
  const { _proxyPort } = internals();
  const port = channel[_proxyPort];
  if (getMessagePortId(port) === null) return;
  port.postMessage(message);
}

// send(), close() and the bufferedAmountLowThreshold setter of a received
// channel (01_webrtc.js), run by the realm that has the connection.
function proxySend(channel, text, bytes) {
  postToOwner(channel, { t: "send", text, bytes });
}

function proxyClose(channel) {
  postToOwner(channel, { t: "close" });
}

function proxySetThreshold(channel, value) {
  postToOwner(channel, { t: "threshold", value });
}

return {
  forwardEvent,
  proxyClose,
  proxySend,
  proxySetThreshold,
  transferReceivingSteps,
  transferSteps,
};
})();
