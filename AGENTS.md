# AGENTS.md

## Purpose of this repository

This repo is for building a tiny SIP “phone” on an M5Stack Atom Echo (ESP32-PICO-D4) using **Rust on top of ESP-IDF**.

The goal is not to make a feature-complete softphone, but a deliberately minimal, well-structured, hackable SIP endpoint that:

- Registers with a SIP server on a local network.
- Can place and accept a **single call at a time**.
- Uses **push-to-talk (PTT)** audio to avoid echo.
- Uses the Atom Echo’s **single button** and **neopixel** as the only UI.
- Uses **simple, well-modeled Rust state machines** for SIP and call control.
- Leans on ESP-IDF (FreeRTOS, Wi-Fi, I2S) instead of re-implementing low-level guts.

Agents should optimize for clarity, modularity, and hackability over “maximum features”.

---

## High-level constraints and non-goals

**Do implement now:**

- SIP over UDP (no TCP, no TLS at first).
- Registration with a single SIP account, single server.
- A single active call at a time, no call waiting / transfer / hold / conferencing.
- Basic SDP offer/answer with:
  - One audio stream.
  - A single codec: **G.711 μ-law (PCMU, payload type 0)**.
- RTP send/receive with:
  - Fixed packetization interval (e.g. 20 ms).
  - Simple jitter buffer (e.g. 60–80 ms) with minimal packet-loss handling.
- PTT-style audio handling to avoid echo:
  - Simple half-duplex or near-half-duplex behavior using the button.
- Atom Echo hardware integration:
  - Wi-Fi client mode.
  - I2S input (mic) and output (speaker).
  - One button.
  - One neopixel (RGB LED).
- Configuration via very simple means:
  - Start with **hard-coded config** in Rust constants.
  - Optionally later: simple NVS-backed config set via UART/USB shell.

**Do NOT implement (for now):**

- SIP over TLS, HTTPS, or any form of SRTP/WebRTC.
- Multiple accounts, multiple concurrent calls, or call transfer/hold/park features.
- STUN/ICE, complex NAT traversal, or external “cloud” SIP providers.
  - Assume a local SIP server (e.g. Asterisk/FreePBX) on the same LAN.
- Acoustic echo cancellation (AEC) or advanced DSP.
  - PTT and aggressive muting are acceptable substitutes.
- Complex UI flows, menus, or configuration screens.
  - There is no display; the button and neopixel should be used sparingly.

---

## Target platform and stack

- **MCU**: ESP32-PICO-D4 (i.e., ESP32 “classic” with integrated flash).
- **Hardware environment**:
  - M5Stack Atom Echo dev board.
  - Onboard I2S DAC/amp for speaker.
  - Onboard I2S microphone.
  - One user button.
  - One neopixel RGB LED.

- **Peripheral pinouts**:
  - Speaker audio (NS4168):
    - G22: AMPDATA (SADTA)
    - G19: AMPBCLK (BCLK)
    - G33: SYSLRCLK (LRCK)

  - Micropohone (SPM1423):
    - G33: SYSLRCLK (CLK)
    - G23: MICDATA (DATA)

  - RGB LED (SK6812):
    - G27: DATA

  - Button:
    - G39: Input

- **Software stack**:
  - **Rust** as the primary language.
  - **ESP-IDF** as the underlying SDK:
    - FreeRTOS scheduler.
    - Wi-Fi driver and TCP/IP stack (lwIP).
    - I2S peripheral driver with DMA.
    - Logging, NVS, etc.
  - Rust bindings:
    - `esp-idf-sys` for FFI.
    - `esp-idf-hal` and/or `esp-idf-svc` where practical.

- **Build model**:
  - `std`-enabled Rust (not `no_std`) using `esp-idf` target.
  - Use the esp-rs recommended template and tooling where applicable.

---

## Architectural overview

The code should be structured into logical layers and modules. Agents should favor separation of concerns.

### Suggested crate/module structure

This is a suggested high-level structure; adjust naming as needed but preserve the separation:

- `app/` (binary crate)
  - Bootstraps ESP-IDF runtime.
  - Initializes Wi-Fi.
  - Initializes hardware (I2S, button, neopixel).
  - Spawns FreeRTOS-backed Rust tasks/threads.
  - Wires together modules below.

- `sip_core/` (library crate)
  - SIP protocol primitives:
    - Parsing and generation of minimal SIP messages.
    - Support for: `REGISTER`, `INVITE`, `ACK`, `BYE`, `OPTIONS` (optional), and basic responses.
    - Stateless / transaction-level handling (client and server side, as needed).
  - Simple transaction and dialog state machines:
    - One ongoing registration transaction.
    - One active dialog (or none) at a time.
  - HTTP-like string handling should be encapsulated; avoid sprinkling string manipulation across the app.

- `sdp/` (could be part of `sip_core` or separate)
  - Minimal SDP representation:
    - Session-level fields: origin, connection, timing.
    - Media-level `m=audio` sections.
    - `rtpmap` attributes for G.711 μ-law.
  - Functions to:
    - Generate SDP offers for our audio capabilities.
    - Parse incoming SDP answers/offers and extract:
      - Remote IP.
      - Remote audio port.
      - Chosen payload type and codec parameters.

- `rtp_audio/` (library crate)
  - RTP packet representation (header, payload).
  - RTP send/receive logic:
    - Sequence numbers and timestamps.
    - SSRC handling.
  - Codec support:
    - G.711 μ-law encode/decode from/to 16-bit PCM.
  - Jitter buffer:
    - Simple ring buffer storing a fixed number of 20 ms audio frames.
    - Ability to:
      - Push incoming frames with timestamps.
      - Pop frames for playback at a regular interval.
      - Discard very late packets or fill gaps with silence or last-sample.

- `atom_echo_hw/` (library crate or module)
  - Hardware abstraction specific to Atom Echo + ESP-IDF:
    - Wi-Fi setup and connection management.
    - I2S setup for microphone and speaker (with DMA).
    - Button input handling (debounced).
    - Neopixel control.
    - Optional: simple non-volatile configuration store.
  - Provide a small, clear API to the rest of the app:
    - `fn init_wifi(config: WifiConfig) -> Result<WifiHandle, Error>`
    - `fn init_audio() -> Result<AudioHandle, Error>`
    - `fn read_mic_frame(&mut self, buf: &mut [i16]) -> Result<usize, Error>`
    - `fn write_speaker_frame(&mut self, buf: &[i16]) -> Result<usize, Error>`
    - `fn read_button_state(&self) -> ButtonState`
    - `fn set_led_state(&self, LedState)`

Aim to keep `unsafe` largely contained in `atom_echo_hw` and thin wrappers around ESP-IDF functions.

---

## Task / thread model

The application will run multiple logical tasks on top of FreeRTOS. Agents should design with timing and audio constraints in mind.

A suggested starting point:

1. **Wi-Fi / network initialization**
   - Runs at startup in the main thread.
   - Connects to Wi-Fi.
   - Once we have an IP, spawns the other tasks and continues as:
     - A low-frequency “supervisor” loop, or
     - Simply exits after initialization if not needed.

2. **SIP task**
   - Responsible for:
     - Registering to the SIP server at startup.
     - Periodic re-registration.
     - Handling incoming SIP messages on UDP (REGISTER responses, INVITE, BYE, etc.).
     - Managing call state (idle, ringing, in-call).
     - Informing other subsystems (UI, RTP) about state changes via channels/message queues.
   - Simplifications:
     - One SIP account.
     - One current dialog (call) at most.

3. **RTP send task**
   - Runs periodically (e.g. every 20 ms).
   - On PTT “talk” state:
     - Reads PCM frames from the I2S microphone via `atom_echo_hw`.
     - Encodes audio to G.711 μ-law.
     - Wraps frames in RTP packets.
     - Sends via UDP to the remote RTP endpoint decided by SDP.
   - On receive-only state:
     - Does nothing or sends comfort noise / silence (optional).

4. **RTP receive task**
   - Listens for incoming RTP packets on a designated UDP port.
   - Validates SSRC and payload type.
   - Inserts decoded PCM frames into the jitter buffer.
   - Not responsible for playback timing; it only feeds the jitter buffer.

5. **Audio playback task**
   - Runs on a fixed interval (e.g. every 20 ms).
   - Pops frames from the jitter buffer.
     - If buffer is under-run, plays silence or repeated samples.
   - Writes frames to I2S speaker output via DMA.
   - This task and the I2S/DMA configuration must be high-priority enough to avoid audio glitches.

6. **UI / button / LED task**
   - Polls or receives events from the button driver.
   - Implements the PTT logic and call control logic:
     - Short press on idle: place outbound call to a configured extension.
     - Incoming call:
       - LED indicates ringing.
       - Short press: answer call.
       - Long press: reject call.
     - In-call:
       - Button controls PTT mode:
         - Press (or hold) to switch to “talk” (mic active, speaker muted).
         - Release to switch to “listen” (speaker active, mic muted).
       - Another press (or long press) to hang up.
   - Updates neopixel color/pattern based on:
     - Not registered / error.
     - Registered & idle.
     - Ringing.
     - In-call (talk vs listen).

Agents should design inter-task communication using message queues or channels, not global mutable state. Favor explicit small enums for messages.

---

## Call handling semantics

For now, implement a very minimal call model:

- **Registration:**
  - At startup, register to the SIP server with a configured URI/user/pass.
  - Use SIP Digest authentication (no TLS).
  - Refresh registration before expiry.

- **Outgoing calls:**
  - When user presses the button in idle state:
    - Send `INVITE` to a configured SIP URI (e.g., extension 100).
    - On `180 Ringing`, switch UI to “calling” indication.
    - On `200 OK`, parse SDP, send `ACK`, and move to “in-call” state.
  - Hang up with `BYE`.

- **Incoming calls:**
  - When an `INVITE` arrives:
    - If already in a call, respond with busy or reject.
    - If idle, ring:
      - LED pattern indicates incoming call.
      - Button press accepts, sending `200 OK` with our SDP.
      - Button long press rejects (e.g. `486 Busy Here` or `603 Decline`).

- **In-call:**
  - Only one call at a time.
  - PTT behavior:
    - Speaker and mic are not both active at high volume simultaneously.
    - Implementation detail: when in “talk” mode, reduce or mute speaker playback; when in “listen” mode, mute mic or drop outgoing RTP.
  - Hangup path:
    - Local hangup: send `BYE`, go back to idle.
    - Remote hangup: receive `BYE`, respond appropriately, go back to idle.

---

## Audio behavior and PTT specifics

Agents should implement audio with these simplifying assumptions:

- **Codec**: G.711 µ-law only.
- **Sample rate**:
  - At the SIP/RTP level: 8 kHz.
  - If I2S hardware runs at 16 kHz or 48 kHz, introduce a simple resampler (can be linear) but keep the internal model as 8 kHz frames.
- **Frame size**:
  - 20 ms per RTP packet, i.e. 160 samples at 8 kHz.
- **PTT model**:
  - Only one of:
    - Mic → RTP (talk), or
    - RTP → Speaker (listen)
    is dominant at a time.
  - Minimal/no echo suppression; rely on not mixing mic and speaker at once.

---

## Configuration and assumptions

Keep configuration simple:

- Initially:
  - Hard code:
    - Wi-Fi SSID/password.
    - SIP server IP/port.
    - SIP username and password.
    - Target extension or URI for the outgoing call button.
- Later (optional, but acceptable for agents to implement):
  - A simple UART/USB text interface (REPL-like) for configuration:
    - Commands like `set wifi_ssid`, `set wifi_pass`, `set sip_user`, `set sip_pass`, `set sip_server`, etc.
    - Persist key-value pairs in ESP-IDF NVS.
  - No need for JSON or complex config formats; simple line-based commands are fine.

Assumptions:

- The SIP server is on the same LAN.
- NAT and firewall issues are not the primary focus.
- No TLS or HTTPS is required or desired at this stage.

---

## Coding style and design preferences

Agents should favor:

- **Rust enums and pattern matching** for protocol and state machines.
- **Small, cohesive modules** with clearly named responsibilities.
- Minimal and well-isolated `unsafe` blocks.
- Clear, explicit error handling (`Result<_, Error>` types).
- Log messages that are:
  - Concise.
  - Helpful for debugging SIP and audio timing issues.
- Tests where feasible:
  - Pure-Rust modules like SIP parsing, SDP parsing, RTP packing/unpacking should be testable on the host (x86) with `cargo test`.

Avoid:

- Global mutable state.
- Copying protocol-related strings around unnecessarily; use slices/refs where practical.
- Overengineering configuration (no need for JSON parsers or complex schema systems at this stage).
- Pulling in large, generic SIP stacks unless they can be cleanly wrapped and constrained to the minimal feature set described above.

---

## Testing and host-side development

Where possible, agents should structure code so that core logic can be compiled and tested on a desktop:

- `sip_core`:
  - Unit tests for:
    - Message parsing (requests and responses).
    - Message generation.
    - Transaction/dialog state transitions.
- `sdp`:
  - Parsing known good SDP blobs.
  - Generating our local SDP and verifying structure.
- `rtp_audio`:
  - G.711 encode/decode round-trip tests.
  - RTP header packing/unpacking tests.
  - Jitter buffer behavior: packet reordering, loss, and underflow conditions.

Hardware-dependent modules (`atom_echo_hw`) can be excluded from host builds via feature flags or conditional compilation.

---

## Future extensions (for context, not for immediate implementation)

These are possible future enhancements. Agents should be aware but must not prioritize them over core goals:

- Optional AEC or basic echo suppression using an existing DSP library.
- SIP over TLS and SRTP.
- Extended codecs (Opus, G.722, etc.).
- Multiple configured contacts or “profiles.”
- Integration with a small HTTP configuration page served over Wi-Fi.
- Simple OTA firmware update mechanism.

For now, keep the scope constrained to the minimal SIP phone described above.

---

## Summary for agents

When contributing code or generating files:

- Aim for a clean, layered design:
  - Rust-first protocol logic (SIP, SDP, RTP) in testable crates.
  - Thin hardware/ESP-IDF integration layers.
  - Clear, finite state machines for:
    - Registration.
    - Call handling.
    - UI/PTT behavior.
- Prefer correctness, clarity, and robustness over fancy features.
- Respect the constraints:
  - Local SIP, UDP-only, one account, one call, G.711 only, PTT audio, no TLS/HTTPS.

If in doubt, choose the simpler behavior that fits within these constraints and keeps the system understandable and hackable.

