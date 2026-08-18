FROM mcr.microsoft.com/devcontainers/base:ubuntu-22.04

ARG COCOTB_VERSION=2.0.1

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        g++ \
        make \
        python3-dev \
        python3-pip \
        verilator \
    && python3 -m pip install --no-cache-dir "cocotb==${COCOTB_VERSION}" \
    && rm -rf /var/lib/apt/lists/*
