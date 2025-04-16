# Price Pusher

A blockchain-based pyth price feeding system for decentralized applications

## Overview

Price Pusher is a system that provides real-time price data to blockchain smart contracts. It consists of three main components:

1. `price-pusher-core` - Core price feed aggregation and processing logic
2. `price-pusher-aptos` - Aptos blockchain integration module
3. `price-pusher` - Main CLI application

## Features

- Real-time price data collection
- Blockchain integration
- Decentralized data feeding
- Robust error handling
- Configurable sources

## Setup

To get started:

```bash
cargo build
```

## Usage
Run the application with:
```
cargo run --release
```

## Configuration

Configure the price feeder by editing config.yaml:

```
- alias: BTC/USD
  id: f9c0172ba10dfa4d19088d94f5bf61d3b54d5bd7483a322a982e1373ee8ea31b
  time_difference: 30
  price_deviation: 0.5
  confidence_ratio: 1
- alias: BNB/USD
  id: ecf553770d9b10965f8fb64771e93f5690a182edc32be4a3236e0caaa6e0581a
  time_difference: 30
  price_deviation: 1
  confidence_ratio: 1
  early_update:
    time_difference: 30
    price_deviation: 0.5
    confidence_ratio: 0.1
```
## Contributing

Contributions are welcome! Please fork the repository and submit a pull request.