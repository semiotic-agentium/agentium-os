# Changelog

## [0.2.2](https://github.com/semiotic-agentium/agentium-os/compare/v0.2.1...v0.2.2) (2026-07-10)


### Bug Fixes

* embed host compaction baml ([24d3ba2](https://github.com/semiotic-agentium/agentium-os/commit/24d3ba20c198e71c89bb4687793fce5986483c80))
* embed host compaction baml ([4c79545](https://github.com/semiotic-agentium/agentium-os/commit/4c79545bd22e99225aef59bcff4bae1ed79a57e9))

## [0.2.1](https://github.com/semiotic-agentium/agentium-os/compare/v0.2.0...v0.2.1) (2026-07-06)


### Bug Fixes

* stabilize runtime progress diagnose check ([bf96a6d](https://github.com/semiotic-agentium/agentium-os/commit/bf96a6dce942484cacc88f1d7117dff28771d817))

## [0.2.0](https://github.com/semiotic-agentium/agentium-os/compare/v0.1.3...v0.2.0) (2026-07-05)


### Features

* auto-finish one-shot sessions after entry Send ([b54b845](https://github.com/semiotic-agentium/agentium-os/commit/b54b845942679f4244ddb8c42bb76322c8582dca))
* entry-hop typed Send with tool_name literal ([d025bdb](https://github.com/semiotic-agentium/agentium-os/commit/d025bdb7282b96ec303d8ef47a28d58a9383b481))
* host-owned context compaction and planning transcript projection ([1cae9d5](https://github.com/semiotic-agentium/agentium-os/commit/1cae9d56976b65441b6a32c0c1a27c922a0880f9))
* **provenance:** make drift scoring opt-in, no embedding model by default ([49965ba](https://github.com/semiotic-agentium/agentium-os/commit/49965ba102148bf53e4af822818fd5b853a70e5e))
* **provenance:** make drift scoring opt-in, no embedding model by default ([5c60ebb](https://github.com/semiotic-agentium/agentium-os/commit/5c60ebb88c2e519263b5815ebd587a91bba8a92e))
* settlement-driven compaction with legacy path removal ([b4d7a03](https://github.com/semiotic-agentium/agentium-os/commit/b4d7a03380e21a51155f159117282d032c4cc9bc))
* settlement-driven context compaction with wire-ref validation ([5922fd2](https://github.com/semiotic-agentium/agentium-os/commit/5922fd24f95af289a11dcd648210b3ad76f6a72e))
* unify platform host and SDK under agentium binary ([5325fea](https://github.com/semiotic-agentium/agentium-os/commit/5325fea4f546143b47792554ef395862e3c1a1c1))
* unify platform host and SDK under agentium binary ([22256b5](https://github.com/semiotic-agentium/agentium-os/commit/22256b58fcada3bbf008fb4caf1c7a6cdff7bc36))
* validate compaction summary refs with in-trigger LLM retry ([dca0dbe](https://github.com/semiotic-agentium/agentium-os/commit/dca0dbe79101d7e17484c4cee3f3a52fbe548937))


### Bug Fixes

* align archive ledger context fields ([a4d36e0](https://github.com/semiotic-agentium/agentium-os/commit/a4d36e0a39ab6f1b37c727cf7ed6fc7a18b9bb83))
* align dispatch and compaction tests with main ingress fields ([255c87d](https://github.com/semiotic-agentium/agentium-os/commit/255c87d10ab695a2c6fe7a93068bb8907d428484))
* **ci:** build agentium with stable toolchain for k8s smoke and release dry-run ([4d7d20e](https://github.com/semiotic-agentium/agentium-os/commit/4d7d20ef3aacd65d5e0e598bfb1d02ff51021451))
* **ci:** install libdbus in sandbox-adapter job apt deps ([41d52dc](https://github.com/semiotic-agentium/agentium-os/commit/41d52dc8b356bee83e3d2b811489ff9d42604af9))
* **ci:** repair agentium path typos and install libdbus in CI runners ([22f113f](https://github.com/semiotic-agentium/agentium-os/commit/22f113f463a958c88e778073d0b5e4f6005c79e1))
* **ci:** restore test runner bin, Docker libdbus deps, eval clippy ([37df683](https://github.com/semiotic-agentium/agentium-os/commit/37df683bdd9aa59e6ce6921c6d2f52ca03897017))
* **ci:** stable toolchain for sandbox parity and earlier k8s agentium build ([79f849f](https://github.com/semiotic-agentium/agentium-os/commit/79f849f4683fdca860a15727960488d239b4c139))
* clippy and fmt ([cbb5824](https://github.com/semiotic-agentium/agentium-os/commit/cbb58246abf1e64c4a6a4dc3b7784e20b74cc8b8))
* explicit Finish/Abort on a closed session is a no-op ([7193a3b](https://github.com/semiotic-agentium/agentium-os/commit/7193a3bddb267533fe308305aae54a621b1a78fa))
* harden direct send step executor flow ([2dfeb86](https://github.com/semiotic-agentium/agentium-os/commit/2dfeb86554690c0b688ceab7a697c80b76c14f4a))
* hide one-shot lifecycle ops from tool catalog ([9b05b11](https://github.com/semiotic-agentium/agentium-os/commit/9b05b117faa0fe880c0329b2a760f606233b28a6))
* intent-level failures and no dangling session on auto-opened Send ([79bf9d7](https://github.com/semiotic-agentium/agentium-os/commit/79bf9d7721d352eff1507064e1e9d899233bcd7f))
* isolate MCP one-shot logical sessions ([ae3be8f](https://github.com/semiotic-agentium/agentium-os/commit/ae3be8f7067f1a0d34a53e88decdab157d03ff3a))
* **k8s:** drop duplicate serve arg from runner StatefulSet ([1461683](https://github.com/semiotic-agentium/agentium-os/commit/1461683befac6c8746768acc5846b0e24cee32d7))
* preserve BAML clients without stored LLM config ([507bb73](https://github.com/semiotic-agentium/agentium-os/commit/507bb7355b92f665f544a46f13c1c62db35d6d61))
* ship host compaction BAML in runner container image ([1902630](https://github.com/semiotic-agentium/agentium-os/commit/1902630f1ea109e4f64c279b03a6fa18c68c9bfc))
* **test:** use in-memory deployment state in runner lib unit tests ([8f9d876](https://github.com/semiotic-agentium/agentium-os/commit/8f9d8766d5b80c9edf0901f15705afd8d8bffd7a))
* update openapi snapshot for crate version 0.1.3 ([8e6434a](https://github.com/semiotic-agentium/agentium-os/commit/8e6434ac52a5ed03c1234afb3b576734b71528f7))

## [0.1.3](https://github.com/semiotic-agentium/agentium-os/compare/v0.1.2...v0.1.3) (2026-06-26)


### Bug Fixes

* test release please ([dbc9e95](https://github.com/semiotic-agentium/agentium-os/commit/dbc9e9536c0109f308d2a4b24df5fc95efc4cd43))
* test release please ([7e98204](https://github.com/semiotic-agentium/agentium-os/commit/7e9820486de39eae5af532e1b2416082bc10894f))
