# dev/meteo-tool

MCP stdio toolset for weather forecasts from Open-Meteo. The toolset is version
2.0.0 and keeps the existing `dev/meteo-tool` name and input/output contract.

For the complete local build, cleanup, publish, deploy, and chat flow, run
`./run-meteo-agent.sh` from the repository root. It reuses an existing ready
runner and does not manage the local Docker registry.

## Develop

Run these commands from the repository root:

```bash
cd ~/Documents/Projects/agentium-os
cd examples/external-tools/meteo-tool
npm install
cd ../../..

cargo run -q -p karakara-os -- tool build examples/external-tools/meteo-tool
cargo run -q -p karakara-os -- tool test \
  examples/external-tools/meteo-tool \
  --input '{"location_query":"Athens, Greece","hourly_limit":2}'
```

The tool asks Open-Meteo's geocoding API for a location and then requests the
current conditions and one day of hourly data. Network access is declared in
`karakara.toml`, not in the handler.

## Publish and deploy

The Sandbox runner needs a digest-pinned runtime image that the microsandbox
image loader can pull. For local Linux/KVM validation, use a local registry.
Run this once from the repository root:

```bash
cd "$(git rev-parse --show-toplevel)"

# Build the supervisor-compatible Node runtime image.
just tool-runtime-image

# Expose the image to microsandbox through a local OCI registry.
docker rm -f karakara-tool-registry 2>/dev/null || true
docker run -d --restart=unless-stopped \
  --name karakara-tool-registry \
  -p 5001:5000 \
  registry:2

docker tag karakara/tool-runtime:node-22 \
  localhost:5001/karakara/tool-runtime:node-22
docker push localhost:5001/karakara/tool-runtime:node-22

RUNTIME_DIGEST="$(docker image inspect \
  localhost:5001/karakara/tool-runtime:node-22 \
  --format '{{range .RepoDigests}}{{println .}}{{end}}' \
  | awk -F@ '$1 == "localhost:5001/karakara/tool-runtime" { print $2; exit }')"
export KARAKARA_KVM_IMAGE="localhost:5001/karakara/tool-runtime@$RUNTIME_DIGEST"
printf 'Using runtime image: %s\n' "$KARAKARA_KVM_IMAGE"
```

If you already have a registry-published runtime image, skip the local registry
steps and export its digest-pinned reference instead:

```bash
export KARAKARA_KVM_IMAGE='registry.example.com/karakara/tool-runtime@sha256:...'
```

Start the runner in one terminal. This keeps provenance in `provenance.db`:

```bash
cd "$(git rev-parse --show-toplevel)"
KARAKARA_KVM_IMAGE="$KARAKARA_KVM_IMAGE" just runner-provenance
```

In a second terminal, publish the tool:

```bash
cd "$(git rev-parse --show-toplevel)"
cargo run -q -p karakara-os -- tool publish \
  examples/external-tools/meteo-tool \
  --repository-url http://127.0.0.1:18080/repository \
  --approved-by local-dev \
  --yes
```

Then deploy the agent:

```bash
cargo run -q -p karakara-os -- install agent \
  --path examples/agents/meteo-agent \
  --repository-url http://127.0.0.1:18080/repository \
  --url http://127.0.0.1:18080 \
  --rationale "meteo v2 validation"
```

Publishing builds the MCP server, records the descriptor returned by
`tools/list`, creates the `.ktool` bundle, and stores it in the repository.

## Chat and inspect provenance

After deployment, connect to the agent:

```bash
cargo run -q -p karakara-os -- chat \
  --agent meteo-agent \
  --url http://127.0.0.1:18080
```

The runner exposes provenance queries over HTTP:

```bash
curl 'http://127.0.0.1:18080/provenance/tool-calls?agent_package=meteo-agent&page_size=100'
curl 'http://127.0.0.1:18080/provenance/lifecycle-events?agent_package=meteo-agent&page_size=100'
```

The local provenance store is `provenance.db`. OpenTelemetry connection
warnings are expected when no collector is running.
