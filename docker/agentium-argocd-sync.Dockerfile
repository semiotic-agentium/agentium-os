# Ephemeral image: workspace copy of deploy/helm + deploy/values for `argocd app sync --local` inside a Job.
ARG ARGOCD_BASE=quay.io/argoproj/argocd
ARG ARGOCD_TAG=v2.13.3
FROM ${ARGOCD_BASE}:${ARGOCD_TAG}
WORKDIR /workspace
COPY deploy/helm /workspace/deploy/helm
COPY deploy/values /workspace/deploy/values
