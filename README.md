# Drift Gateway

Self hosted API gateway to easily interact with Drift V2 Protocol

## Build & Run

```bash
# build
cargo build --release

# configure the gateway signing key
export DRIFT_GATEWAY_KEY=</PATH/TO/KEY.json | seedBase58>

# '--dev' to toggle devnet markets (default is mainnet)
# ensure the RPC node is also using the matching devnet or mainnet
drift-gateway --dev https://api.devnet.solana.com

# or mainnet
# NB: `api.mainnet-beta.solana.com` cannot be used due to rate limits on certain RPC calls
drift-gateway https://rpc-provider.example.com
```

with docker
```bash
docker build -f Dockerfile . -t drift-gateway
docker run -e DRIFT_GATEWAY_KEY=<BASE58_SEED> -p 8080:8080 drift-gateway https://api.mainnet-beta.solana.com --host 0.0.0.0
```

## Usage
```bash
Usage: drift-gateway <rpc_host> [--dev] [--host <host>] [--port <port>] [--delegate <delegate>] [--emulate <emulate>]

Drift gateway server

Positional Arguments:
  rpc_host          the solana RPC URL

Options:
  --dev             run in devnet mode
  --host            gateway host address
  --port            gateway port
  --delegate        use delegated signing mode, provide the delegator pubkey
  --emulate         run the gateway in read-only mode for given authority pubkey
  --help            display usage information
```

## API Examples

Please refer to https://drift-labs.github.io/v2-teacher/ for further examples and reference documentation on various types, fields, and operations available on drift.

### Get Market Info
gets info on all available spot & perp markets

NB: spot marketIndex `0`/USDC is non-tradable
```bash
$ curl localhost:8080/v2/markets
```

**Response**
- `priceStep` smallest order price increment for the market
- `amountStep` smallest order amount increment for the market
- `minOrderSize` minimum order amount for the market

```json
{
  "spot": [
    {
      "marketIndex": 1,
      "symbol": "SOL",
      "priceStep": "0.0001",
      "amountStep": "0.1",
      "minOrderSize": "0.1"
    },
    // ...
  ],
  "perp": [
    {
      "marketIndex": 0,
      "symbol": "SOL-PERP",
      "priceStep": "0.0001",
      "amountStep": "0.01",
      "minOrderSize": "0.01"
    },
    // ...
  ]
}
```

### Get Orderbook
gets a full snapshot of the current orderbook for a given market

- `marketType` - "spot" or "perp

```bash
$ curl localhost:8080/v2/orderbook -X GET \
  -H 'content-type: application/json' \
  -d '{"marketIndex":0,"marketType":"perp"}'
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
$ curl -X GET \
  -H 'content-type: application/json' \
  -d '{"marketIndex":1,"marketType":"spot"}' \
  localhost:8080/v2/orders
```

**Response**
```json
{
  "orders": [
    {
      "orderType": "limit",
      "marketIndex": 1,
      "marketType": "spot",
      "amount": "-1.100000000",
      "filled": "0.000000000",
      "price": "80.500000",
      "postOnly": true,
      "reduceOnly": false,
      "userOrderId": 101,
      "orderId": 35,
      "immediateOrCancel": false
    },
    {
      "orderType": "limit",
      "marketIndex": 1,
      "marketType": "perp",
      "amount": "0.005000000",
      "filled": "0.000000000",
      "price": "0.000000",
      "postOnly": true,
      "reduceOnly": false,
      "userOrderId": 103,
      "orderId": 50,
      "immediateOrCancel": false,
      "oraclePriceOffset": "20.000000"
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
$ curl -X GET \
  -H 'content-type: application/json' \
  -d '{"marketIndex":0,"marketType":"perp"}' \
localhost:8080/v2/positions
```

**Response**
```json
{
  "spot": [
    {
      "amount": "0.400429",
      "type": "deposit",
      "marketIndex": 0
    },
    {
      "amount": "9.971961702",
      "type": "deposit",
      "marketIndex": 1
    }
  ],
  "perp": []
}
```

### Place Orders

- use sub-zero `amount` to indicate sell/offer order
- `userOrderId` is a uint in the range 1 <= x <= 255 which can be assigned by the client to help distinguish orders
- `orderType` only "limit" and "market" options are fully supported by the gateway
- `oraclePriceOffset` supported on `"limit"` order types.
It creates a limit order with a floating price relative to the market oracle price. when supplied the `price` field is ignored.
- `maxTs` order expiration timestamp. NB: expired orders can incur protocol costs
```bash
$ curl localhost:8080/v2/orders -X POST \
-H 'content-type: application/json' \
-d '{
    "orders": [
    {
        "marketIndex": 1,
        "marketType": "spot",
        "amount": -1.23,
        "price": 80.0,
        "postOnly": true,
        "orderType": "limit",
        "userOrderId": 101,
        "immediateOrCancel": false,
        "reduceOnly": false,
        "maxTs": 1707112301
    },
    {
        "marketIndex": 0,
        "marketType": "perp",
        "amount": 1.23,
        "postOnly": true,
        "orderType": "limit",
        "oraclePriceOffset": 2,
        "userOrderId": 102
    }]
}'
```
Returns solana tx signature on success

### Modify Orders
like place orders but caller must use either `orderId` or `userOrderId` to indicate which order(s) to modify.

- `amount` can be modified to flip the order from long/short to bid/ask
- the order market cannot be modified.
```bash
$ curl localhost:8080/v2/orders -X PATCH \
-H 'content-type: application/json' \
-d '{
    "orders": [{
        "marketIndex": 0,
        "marketType": "perp",
        "amount": -1.1,
        "price": 80.5,
        "userOrderId": 101
    },
    {
        "marketIndex": 1,
        "marketType": "spot",
        "amount": 2.05,
        "price": 61.0,
        "userOrderId": 32
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

### Atomic Cancel/Modify/Place Orders

Atomically cancel, modify, and place orders without possible downtime.
Request format is an embedded cancel modify, and place request

```bash
$ curl localhost:8080/v2/orders/cancelAndPlace -X POST -H 'content-type: application/json' \
-d '{
    "cancel": {
        "marketIndex": 0,
        "marketType": "perp"
    },
    "modify": {
      "orders": [{
            "marketIndex": 0,
            "marketType": "perp",
            "orderId": 555,
            "amount": -0.5,
            "price": 82.0
      }]
    },
    "place": {
        "orders": [
        {
            "marketIndex": 0,
            "marketType": "perp",
            "amount": -1.23,
            "price": 99.0,
            "postOnly": true,
            "orderType": "limit",
            "immediateOrCancel": false,
            "reduceOnly": false
        }]
    }
}'
```

## WebSockets
Websocket API is provided for live event streams by default at port `127.0.0.1:1337`

## Subscribing
Subscribe to order and fills updates by a `subAccountId` (`0` is the drift default)
```ts
{"method":"subscribe", "subAccountId":0}
// unsubscribe
{"method":"unsubscribe", "subAccountId":0}
```

## Event Payloads

event payloads can be distinguished by "channel" field and the "data" payload is keyed by the event type

**order cancelled**
```json
{
    "data": {
        "orderCancel": {
            "orderId": 156,
            "ts": 1704777451,
            "signature": "2Cdo5Xgxj6uWY6dnWmuU5a8tH5fKC2K6YUqzVYKgnm8KkMVhPczBZrNEs4VGwEBMhgosifmNjBXSjFMWbGKJiqSz"
        }
    },
    "channel": "orders",
    "subAccountId": 0
}
```

**order expired**
- if an order's `maxTs` is reached then it can be cancelled by protocol keeper bots, producing the following expired event.  
```json
{
    "data": {
        "orderExpire": {
            "orderId": 156,
            "fee": "-0.0012",
            "ts": 1704777451,
            "signature": "2Cdo5Xgxj6uWY6dnWmuU5a8tH5fKC2K6YUqzVYKgnm8KkMVhPczBZrNEs4VGwEBMhgosifmNjBXSjFMWbGKJiqSz"
        }
    },
    "channel": "orders",
    "subAccountId": 0
}
```

**order created**
- auction and trigger fields are only relevant for auction type or trigger type orders respectively.
- `price` is shown as `0` for market and oracle orders.
```json
{
   "data": {
        "orderCreate": {
            "order": {
                "slot": 271243169,
                "price": "0",
                "amount": "0.1",
                "filled": "0",
                "triggerPrice": "0",
                "auctionStartPrice": "0",
                "auctionEndPrice": "0",
                "maxTs": 0,
                "oraclePriceOffset": "2",
                "orderId": 157,
                "marketIndex": 0,
                "orderType": "limit",
                "marketType": "perp",
                "userOrderId": 102,
                "direction": "buy",
                "reduceOnly": false,
                "postOnly": false,
                "immediateOrCancel": false,
                "auctionDuration": 0
            },
            "ts": 1704777347,
            "signature": "2Cdo5Xgxj6uWY6dnWmuU5a8tH5fKC2K6YUqzVYKgnm8KkMVhPczBZrNEs4VGwEBMhgosifmNjBXSjFMWbGKJiqSz"
        }
    },
    "channel": "orders",
    "subAccountId": 0
}
```

**order fill**

- `fee`: positive amounts are (maker) rebates

```json
{
    "data": {
        "fill": {
            "side": "buy",
            "fee": "0.002581",
            "amount": "0.1",
            "price": "103.22087",
            "orderId": 157,
            "ts": 1704777355,
            "signature": "2Cdo5Xgxj6uWY6dnWmuU5a8tH5fKC2K6YUqzVYKgnm8KkMVhPczBZrNEs4VGwEBMhgosifmNjBXSjFMWbGKJiqSz"
        }
    },
    "channel": "fills",
    "subAccountId": 0
}
```

**order modify**

Modifying an order produces a cancel event followed by a create event with the same orderId


**order cancel (missing) | experimental**

emitted when a cancel action was requested on an order that did not exist onchain.

this event may be safely ignored, it is added in an effort to help order life-cycle tracking in certain setups.

- one of `userOrderId` or `orderId` will be a non-zero value (dependent on the original tx).

```json
{
    "data": {
        "orderCancelMissing": {
            "userOrderId": 5,
            "orderId": 0,
            "ts": 1704777451,
            "signature": "2Cdo5Xgxj6uWY6dnWmuU5a8tH5fKC2K6YUqzVYKgnm8KkMVhPczBZrNEs4VGwEBMhgosifmNjBXSjFMWbGKJiqSz"
        }
    },
    "channel": "orders",
    "subAccountId": 0
}
```

## Emulation Mode
Passing the `--emulate <EMULATED_PUBBKEY>` flag will instruct the gateway to run in read-only mode.

The gateway will receive all events, positions, etc. as normal but be unable to send transactions.

note therefore `DRIFT_GATEWAY_KEY` is not required to be set.


## Delegated Signing Mode
Passing the `--delegate <DELEGATOR_PUBKEY>` flag will instruct the gateway to run in delegated signing mode.

In this mode, the gateway will act for `DELEGATOR_PUBKEY` and sub-accounts while signing with the key provided via `DRIFT_GATEWAY_KEY` (i.e delegate key).

Use the drift UI or Ts/Python SDK to assign a delegator key.
see [Delegated Accounts](https://docs.drift.trade/delegated-accounts) for more information.

## Sub-account Switching
By default the gateway uses the drift sub-account (index 0)

A `subAccountId` URL query parameter may be supplied to switch the sub-account per request basis.

e.g `http://<gateway>/v1/orders?subAccountId=3` will return orders for the wallet's sub-account 3

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

### Common Errors
`AccountNotFound` usually means the drift user (sub)account has not been initialized.
Use the UI or Ts/Python sdk to initialize the sub-account first.
```json
{
  "code": 500,
  "reason": "AccountNotFound: pubkey=FQHZg9yU2o5uN9ERQyeTNNAMe3JWf13gce2DUj6x2HTv"
}
```

The free _api.mainnet-beta.solana.com_RPC cannot be used due to rate-limits on `getProgramAccounts` calls
```rust
Some(GetProgramAccounts), kind: Reqwest(reqwest::Error { kind: Status(410), ...
```
