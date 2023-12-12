# Drift Gateway

Self hosted API gateway to easily interact with Drift V2 Protocol

## Run

```bash
export `DRIFT_GATEWAY_KEY=</PATH/TO/KEY | keyBase58>`

# --dev to use devnet markets (default is mainnet)
# ensure the RPC node is also using the matching dev or mainnet
drift-gateway --dev  https://api.devnet.solana.com
```

```bash
Usage: drift-gateway <rpc_host> [--dev] [--host <host>] [--port <port>]

Drift gateway server

Positional Arguments:
  rpc_host          the solana RPC URL

Options:
  --dev             run in devnet mode
  --host            gateway host address
  --port            gateway port
  --help            display usage information
```

## Examples

Place Orders
```bash
~: curl localhost:8080/v2/orders -X POST -H 'content-type: application/json' -d '{
    "orders": [{
        "marketId": 1,
        "marketType": "spot",
        "amount": 100000000,
        "price": 40000000,
        "postOnly": true,
        "orderType": "limit"
    },
    {
        "marketId": 0,
        "marketType": "perp",
        "amount": -100000000,
        "price": 70000000,
        "postOnly": true,
        "orderType": "limit"
    }]
}'
```

Stream Orderbook
```
curl localhost:8080/v2/orderbooks -N -X GET -H 'content-type: application/json' -d '{"market":{"id":3,"type":"perp"}}'
```