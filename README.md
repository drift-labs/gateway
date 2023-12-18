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

## API Examples

Please refer to https://drift-labs.github.io/v2-teacher/ for further examples and reference documentation on various types, fields, and operations available on drift.

### Get Market Info
gets info on all available spot & perp markets
```bash
$ curl localhost:8080/v2/markets
```

**Response**
```json
{
  "spot": [
    {
      "marketIndex": 0,
      "symbol": "USDC",
      "precision": 6
    },
    // ...
  ],
  "perp": [
    {
      "marketIndex": 0,
      "symbol": "SOL-PERP",
      "precision": 6
    },
  ]
    // ...
}
```

### Get Orderbook
gets a full snapshot of the current orderbook for a given market
```bash
$ curl localhost:8080/v2/orderbook -X GET -H 'content-type: application/json' -d '{"marketIndex":0,"marketType":"perp"}'
```

**Response**
```json
{
  "slot": 266118166,
  "bids": [
    {
      "price": "53.616300",
      "amount": "7.110000000"
    },
    {
      "price": "47.014300",
      "amount": "2.000000000"
    },
    {
      "price": "20.879800",
      "amount": "12.160000000"
    }
  ],
  "asks": [
    {
      "price": "80.000000",
      "amount": "1.230000000"
    },
    {
      "price": "120.015569",
      "amount": "1.000000000"
    }
  ]
}
```

to stream orderbooks via websocket public DLOB servers are available at:
- devnet: `wss://master.dlob.drift.trade/ws`
- mainnet: `wss://dlob.drift.trade/ws`
see https://github.com/drift-labs/dlob-server/blob/master/example/wsClient.ts for usage example

### Get Orders
get all orders
```bash
$ curl localhost:8080/v2/orders
```
get orders by market
```bash
$ curl localhost:8080/v2/orders -X GET -H 'content-type: application/json' -d '{"marketIndex":1,"marketType":"spot"}'
```

**Response**
```json
{
  "orders": [
    {
      "order_type": "limit",
      "market_id": 1,
      "market_type": "spot",
      "amount": "-1.100000000",
      "filled": "0.000000000",
      "price": "80.500000",
      "post_only": true,
      "reduce_only": false,
      "user_order_id": 101,
      "order_id": 35,
      "immediate_or_cancel": false
    },
    {
      "order_type": "limit",
      "market_id": 0,
      "market_type": "perp",
      "amount": "-1.230000000",
      "filled": "0.000000000",
      "price": "80.000000",
      "post_only": true,
      "reduce_only": false,
      "user_order_id": 0,
      "order_id": 37,
      "immediate_or_cancel": false
    }
  ]
}
```

### Get Positions
get all positions
```bash
$ curl localhost:8080/v2/positions
```
get positions by market
```bash
$ curl localhost:8080/v2/positions -X GET -H 'content-type: application/json' -d '{"marketIndex":0,"marketType":"perp"}'
```

```json
{
  "spot": [
    {
      "amount": "0.400429",
      "type": "deposit",
      "market_id": 0
    },
    {
      "amount": "9.971961702",
      "type": "deposit",
      "market_id": 1
    }
  ],
  "perp": []
}
```

### Place Orders

- use sub-zero `amount` to indicate sell/offer order
- `userOrderId` is a uint in the range 1 <= x <= 255 which can be assigned by the client to help distinguish orders
- `orderType` only "limit" and "market" options are fully supported by the gateway

```bash
$ curl localhost:8080/v2/orders -X POST -H 'content-type: application/json' -d '{
    "orders": [
    {
        "marketIndex": 1,
        "marketType": "spot",
        "amount": -1.23,
        "price": 80.0,
        "postOnly": true,
        "orderType": "limit",
        "userOrderId": 101
        "immediateOrCancel": false,
        "reduce_only": false,
    },
    {
        "marketIndex": 0,
        "marketType": "perp",
        "amount": 1.23,
        "price": 60.0,
        "postOnly": true,
        "orderType": "limit",
        "userOrderId": 102
    }]
}'
```
Returns solana tx signature on success

### Modify Orders
like place orders but caller must specify either `orderId` or `userOrderId` to indicate which order to modify.

- `amount` can be modified to flip the order from long/short to bid/ask
- the order market cannot be modified.
```bash
$ curl localhost:8080/v2/orders -X PATCH -H 'content-type: application/json' -d '{
    "orders": [{
        "amount": -1.1,
        "price": 80.5,
        "userOrderId": 101
    },
    {
        "amount": 1.05,
        "price": 61.0,
        "orderId": 32
    }]
}'
```
Returns solana tx signature on success

### Cancel Orders
```bash
# cancel all by market id
$ curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"marketIndex":1,"marketType":"spot"}}'
# cancel by order ids
$ curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"ids":[1,2,3,4]}'
# cancel by user assigned order ids
$ curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"userIds":[1,2,3,4]}'
# cancel all orders
$ curl localhost:8080/v2/orders -X DELETE
```
Returns solana tx signature on success

### Cancel and Place Orders

Atomically cancel then place orders without possible downtime.
Request format is an embedded cancel and place request

```bash
$ curl localhost:8080/v2/orders/cancelAndPlace -X POST -H 'content-type: application/json' -d '{
    "cancel": {
        "marketIndex": 0,
        "marketType": "perp"
    },
    "place": {
        "orders": [
        {
            "marketIndex": 0,
            "marketType": "perp",
            "amount": -1.23,
            "price": 80.0,
            "postOnly": true,
            "orderType": "limit",
            "immediateOrCancel": false,
            "reduce_only": false
        }]
    }
}'
```

### Errors
error responses have the following JSON structure:
```json
{
    "code": "<http status code | program error code>",
    "reason": "<explanation>"
}
```

Some endpoints send transactions to the drift program and can return program error codes.  
The full list of drift program error codes is available in the [API docs](https://drift-labs.github.io/v2-teacher/#errors)  