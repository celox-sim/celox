FROM ubuntu:26.04
RUN apt-get update && apt-get install -y --no-install-recommends \
    verilator \
    g++ \
    make \
    libbenchmark-dev \
    && rm -rf /var/lib/apt/lists/*
