"use strict";

function encodeMessage(message) {
  const body = JSON.stringify(message);
  return `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;
}

function createFrameReader(stream) {
  let buffer = Buffer.alloc(0);
  const waiters = [];

  stream.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    drain();
  });
  stream.on("end", () => {
    while (waiters.length > 0) {
      waiters.shift().reject(new Error("stream ended before next MCP frame"));
    }
  });
  stream.on("error", (error) => {
    while (waiters.length > 0) {
      waiters.shift().reject(error);
    }
  });

  function drain() {
    while (waiters.length > 0) {
      const frame = tryReadFrame();
      if (!frame) {
        return;
      }
      waiters.shift().resolve(frame);
    }
  }

  function tryReadFrame() {
    const headerEnd = buffer.indexOf("\r\n\r\n");
    if (headerEnd === -1) {
      return null;
    }
    const header = buffer.slice(0, headerEnd).toString("utf8");
    const match = header.match(/Content-Length:\s*(\d+)/i);
    if (!match) {
      throw new Error(`MCP frame missing Content-Length header: ${header}`);
    }
    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    const bodyEnd = bodyStart + length;
    if (buffer.length < bodyEnd) {
      return null;
    }
    const body = buffer.slice(bodyStart, bodyEnd).toString("utf8");
    buffer = buffer.slice(bodyEnd);
    return JSON.parse(body);
  }

  return {
    nextFrame(timeoutMs = 10000) {
      const frame = tryReadFrame();
      if (frame) {
        return Promise.resolve(frame);
      }
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          reject(new Error(`timed out waiting ${timeoutMs}ms for MCP frame`));
        }, timeoutMs);
        waiters.push({
          resolve(value) {
            clearTimeout(timer);
            resolve(value);
          },
          reject(error) {
            clearTimeout(timer);
            reject(error);
          }
        });
      });
    }
  };
}

module.exports = { createFrameReader, encodeMessage };
