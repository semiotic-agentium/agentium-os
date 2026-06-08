# syntax=docker/dockerfile:1.7

# Sandbox adapter image scaffold for dev/meteo-tool (python source as tool logic).

FROM python:3.12-slim

WORKDIR /opt/tool
COPY main.py /opt/tool/main.py
COPY tool-schema.json /opt/tool/tool-schema.json
COPY adapter/tool-adapter /tool-adapter
COPY adapter/sidecars/etc/agentium/tool-bundle.json /etc/agentium/tool-bundle.json

RUN chmod +x /tool-adapter /opt/tool/main.py

ENTRYPOINT ["/tool-adapter"]
