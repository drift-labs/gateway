# Drift Gateway

Self hosted API gateway to easily interact with Drift V2 Protocol

## Build & Run

```bash
# build
cargo build --release

# configure the gateway wallet key
export DRIFT_GATEWAY_KEY=</PATH/TO/KEY.json | seedBase58>

# '--dev' to toggle devnet markets (default is mainnet)
# ensure the RPC node is also using the matching devnet or mainnet
drift-gateway --dev  https://api.devnet.solana.com

# or mainnet
drift-gateway https://api.mainnet-beta.solana.com
```

with docker
```bash
docker build -f Dockerfile . -t drift-gateway
docker run -e DRIFT_GATEWAY_KEY=<BASE58_SEED> -p 8080:8080 drift-gateway https://api.mainnet-beta.solana.com --host 0.0.0.0
```

## Usage
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

### Get Market Info
```bash
$> curl localhost:8080/v2/markets
```

### Get Orderbook
```bash
$> curl localhost:8080/v2/orderbook -X GET -H 'content-type: application/json' -d '{"market":{"id":0,"type":"perp"}}'
```
to stream orderbooks via websocket DLOB servers are available at:
devnet: `wss://master.dlob.drift.trade/ws`
mainnet: `wss://dlob.drift.trade/ws`
see https://github.com/drift-labs/dlob-server/blob/master/example/wsClient.ts for usage example

### Get Orders
get all orders
```bash
$> curl localhost:8080/v2/orders
```
get orders by market
```bash
$> curl localhost:8080/v2/orders -X GET -H 'content-type: application/json' -d '{"market":{"id":0,"type":"perp"}};
```

### Get Positions
get all positions
```bash
$> curl localhost:8080/v2/positions
```
get positions by market
```bash
$> curl localhost:8080/v2/positions -X GET -H 'content-type: application/json' -d '{"market":{"id":0,"type":"perp"}};
```

### Place Orders
```bash
$> curl localhost:8080/v2/orders -X POST -H 'content-type: application/json' -d '{
    "orders": [{
        "marketId": 1,
        "marketType": "spot",
        "amount": 1.23,
        "price": 40.55,
        "postOnly": true,
        "orderType": "limit",
        "userOrderId": 101
    },
    {
        "marketId": 0,
        "marketType": "perp",
        "amount": -1.05,
        "price": 80,
        "postOnly": true,
        "orderType": "limit",
        "userOrderId": 102
    }]
}'
```

### Modify Orders
like place orders but specify either `orderId` or `userOrderId` to indicate which order to modify
```bash
$> curl localhost:8080/v2/orders -X PATCH -H 'content-type: application/json' -d '{
    "orders": [{
        "marketId": 1,
        "marketType": "spot",
        "amount": 1.23,
        "price": 40.55,
        "postOnly": true,
        "orderType": "limit",
        "userOrderId": 5
    },
    {
        "orderId": 555,
        "marketId": 0,
        "marketType": "perp",
        "amount": -1.05,
        "price": 80,
        "postOnly": true,
        "orderType": "limit"
    }]
}'
```

### Cancelling Orders
```bash
# cancel by market id
$> curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"market":{"id":1,"type":"perp"}}'
# cancel by order ids
$> curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"ids":[1,2,3,4]}'
# cancel by user assigned order ids
$> curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"userIds":[1,2,3,4]}'
# cancel all orders
$> curl localhost:8080/v2/orders -X DELETE
```