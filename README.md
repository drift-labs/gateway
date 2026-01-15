# Drift Gateway

Self hosted API gateway to easily interact with Drift V2 Protocol

## Table of Contents
1. [Build & Run](#build--run)
    - [From Source](#from-source)
    - [From Docker](#from-docker)
2. [Usage](#usage)
    - [Environment Variables](#environment-variables)
    - [Delegated Signing Mode](#delegated-signing-mode)
    - [Sub-account Switching](#sub-account-switching)
    - [Emulation Mode](#emulation-mode)
    - [Transaction Confirmation](#transaction-confirmtaion-and-ttl)
    - [CU price/limits](#cu-price--limits)
3. [API Examples](#api-examples)
    - [HTTP API](#http-api)
      - [`GET` Market Info](#get-market-info)
      - [`GET` Orderbook](#get-orderbook)
      - [`GET` Orders](#get-orders)
      - [`GET` Positions](#get-positions)
      - [`GET` Perp Position Info](#get-position-info-perps-only)
      - [`GET` Transaction Events](#get-transaction-events)
      - [`GET` SOL Balance](#get-sol-balance)
      - [`GET` Authority](#get-authority)
      - [`GET` Margin Info](#get-margin-info)
      - [`GET` Leverage](#get-leverage)
      - [`POST` Leverage](#set-leverage)
      - [`GET` Collateral](#get-collateral)
      - [`POST` Place Orders](#place-orders)
      - [`PATCH` Modify Orders](#modify-orders)
      - [`DELETE` Cancel Orders](#cancel-orders)
      - [`POST` Swap](#swap-orders)
      - [`POST` Titan Swap](#titan-swap-orders)
      - [`PUT` Atomic Cancel/Modify/Place Orders](#atomic-cancelmodifyplace-orders)
    - [Websocket API](#websocket-api)
      - [Subscribing](#subscribing)
      - [Event Payloads](#event-payloads)
  4. [Errors](#errors)
  5. [FAQ](#faq)

## Build & Run

⚠️ Before starting, ensure a Drift _user_ account is initialized e.g. via the drift app at https://beta.drift.trade (devnet) or https://app.drift.trade

### From Docker

Use prebuilt image, ghcr.io:
```bash
# authenticate to github container registry
docker login -u <GITHUB_USERNAME> -P <GITHUB_PAT_TOKEN>
# run image
docker run \
  -e DRIFT_GATEWAY_KEY=<BASE58_SEED> \
  -p 8080:8080 \
  --platform linux/x86_64 \
  ghcr.io/drift-labs/gateway https://rpc-provider.example.com --host 0.0.0.0 \
  --markets wbtc,drift
  --extra-rpcs https://api.mainnet-beta.solana.com
```

Build the Docker image:

```bash
# NOTE: '--platform linux/x86_64' ensures the correct memory layout at runtime
# for solana program data types

docker build -f Dockerfile . -t drift-gateway --platform linux/x86_64
```

Run the image:

```bash
docker run \
  -e DRIFT_GATEWAY_KEY=<BASE58_SEED> \
  -e RUST_LOG=info \
  -p 8080:8080 \
  drift-gateway https://api.mainnet-beta.solana.com --host 0.0.0.0 --markets wbtc,drift
```

### From Source

Build:

Supports latest rust stable (⚠️ requires an `x86_64` toolchain)

```bash
# e.g for linux-gnu
rustup install 1.76.0-x86_64-unknown-linux-gnu      # install old toolchain required to build drift-ffi-sys
rustup override set stable-x86_64-unknown-linux-gnu # latest stable x86_64 toolchain

cargo build --release
# On linux. run `ldconfig` to ensure libdrift is linked after build
ldconfig
```

Run:
```bash
# configure the gateway signing key
export DRIFT_GATEWAY_KEY=</PATH/TO/KEY.json | seedBase58>

# '--dev' to toggle devnet markets (default is mainnet)
# ensure the RPC node is also using the matching devnet or mainnet
drift-gateway --dev https://api.devnet.solana.com

# or mainnet
# NB: `api.mainnet-beta.solana.com` is not recommend for production use cases
drift-gateway https://rpc-provider.example.com --markets sol-perp,sol,weth
```

## gRPC mode
Run gateway subscriptions with geyser gRPC updates
```bash
export GRPC_HOST="grpc.example.com"
export GRPC_X_TOKEN="aabbccddeeff112233"
drift-gateway https://rpc-provider.example.com --grpc
```

## Usage

⚠️ Before starting, ensure a Drift _user_ account is initialized e.g. via the drift app at https://beta.drift.trade (devnet) or https://app.drift.trade

### Environment Variables

These runtime environment variables are required:

| Variable            | Description                               | Example Value                |
|---------------------|-------------------------------------------|------------------------------|
| `DRIFT_GATEWAY_KEY` | Path to your key file or seed in Base58. Transactions will be signed with this keypair | `</PATH/TO/KEY.json>` or `seedBase58` |
| `GRPC_HOST` | endpoint for gRPC subscription mode | `https://grpc.example.com`
| `GRPC_X_TOKEN` | authentication token for gRPC subscription mode | `aabbccddeeff112233`
| `INIT_RPC_THROTTLE` | Adds a delay (seconds) between RPC bursts during gateway startup. Useful to avoid 429/rate-limit errors. Can be set to `0`, if RPC node is highspec | `1` |
| `JUPITER_API_KEY` | **Required for `/v2/swap`**. Jupiter API key for swap operations. Get a free key at [portal.jup.ag](https://portal.jup.ag). See [migration guide](https://dev.jup.ag/portal/migrate-from-lite-api) | `your-jupiter-api-key` |
| `JUPITER_API_URL` | (Optional) Override Jupiter API base URL | `https://api.jup.ag/swap/v1` |
| `TITAN_AUTH_TOKEN` | **Required for `/v2/titan-swap`**. Authentication token for Titan API | `your-titan-auth-token` |
| `TITAN_BASE_URL` | (Optional) Titan API base URL | `https://api.titan.exchange` |

```bash
./target/release/drift-gateway --help
Usage: drift-gateway <rpc_host> [--markets <markets>] [--dev] [--host <host>] [--port <port>] [--ws-port <ws-port>] [--keep-alive-timeout <keep-alive-timeout>] [--delegate <delegate>] [--emulate <emulate>] [--tx-commitment <tx-commitment>] [--commitment <commitment>] [--default-sub-account-id <default-sub-account-id>] [--skip-tx-preflight] [--extra-rpcs <extra-rpcs>] [--verbose] [--grpc]

Drift gateway server

Positional Arguments:
  rpc_host          the solana RPC URL

Options:
  --markets         list of markets to trade e.g '--markets sol-perp,wbtc,pyusd'
                    gateway creates market subscriptions for responsive trading
  --dev             run in devnet mode
  --host            gateway host address
  --port            gateway port
  --ws-port         gateway Ws port
  --keep-alive-timeout
                    http keep-alive timeout in seconds
  --delegate        use delegated signing mode provide the delegator's pubkey
                    (i.e the main account) 'DRIFT_GATEWAY_KEY' should be set to
                    the delegate's private key
  --emulate         run the gateway in read-only mode for given authority pubkey
  --tx-commitment   solana commitment level to use for transaction confirmation
                    (default: confirmed)
  --commitment      solana commitment level to use for state updates (default:
                    confirmed)
  --default-sub-account-id
                    default sub_account_id to use as default. Use the new active-sub-accounts param to subscribe to multiple sub accounts. This param will override active-sub-accounts.
  --active-sub-accounts sub accounts to subscribe to. (default: 0)
  --skip-tx-preflight
                    skip tx preflight checks
  --extra-rpcs      extra solana RPC urls for improved Tx broadcast
  --verbose         enable debug logging
  --grpc            use gRPC mode for network subscriptions
  --help, help      display usage information
```

### Delegated Signing Mode

Passing the `--delegate <DELEGATOR_PUBKEY>` flag will instruct the gateway to run in delegated signing mode.

In this mode, the gateway will act for `DELEGATOR_PUBKEY` and sub-accounts while signing with the key provided via `DRIFT_GATEWAY_KEY` (i.e delegate key).

Use the drift UI or Ts/Python SDK to assign a delegator key.
see [Delegated Accounts](https://docs.drift.trade/delegated-accounts) for more information.

### Sub-account Switching

By default the gateway will perform all account operations on sub-account 0, you can overwrite this default by setting the `--default-sub-account-id` flag on startup.

A `subAccountId` URL query parameter may be supplied to switch the sub-account per request basis.

e.g `http://<gateway>/v1/orders?subAccountId=3` will return orders for the wallet's sub-account 3

## Emulation Mode

Passing the `--emulate <EMULATED_PUBKEY>` flag will instruct the gateway to run in read-only mode.

The gateway will receive all events, positions, etc. as normal but be unable to send transactions.

note therefore `DRIFT_GATEWAY_KEY` is not required to be set.

## CU Price & Limits

**CU limit** may be set on transaction request with the query parameter `computeUnitLimit=300000`, the default if unset is `200000`.

**CU price** in micro-lamports may be set on transaction request with the query parameter `computeUnitPrice=1000`, the default if unset is a dynamic value from chain set at 90-th percentile of the local fee market.  

The following error is logged when a tx does not have enough CU limit, increasing the cu limit can fix it or reducing number complexity of the order e..g number of orders/markets per batch.

```bash
"Program dRiftyHA39MWEi3m9aunc5MzRF1JYuBsbn6VPcn33UH failed: exceeded CUs meter at BPF instruction"]), accounts: None, units_consumed: Some(1000), return_data: None }) })
```

**example request**

```bash
$ curl 'localhost:8080/v2/orders?computeUnitLimit=300000&computeUnitPrice=1000' -X POST \
-H 'content-type: application/json' \
-d # { order data ...}
```

## Transaction Confirmation and TTLs

Gateway endpoints that place network transactions will return the signature as a base64 string.  
User's can poll `transactionEvent` to confirm success by signature or watch Ws events for e.g. confirmation by order Ids instead.  

Gateway will resubmit txs until they are either confirmed by the network or timeout.  
This allows gateway txs to have a higher chance of confirmation during busy network periods.  
setting `?ttl=<TIMEOUT_IN_SECS>` on a request determines how long gateway will resubmit txs for, (default: 4s/~10 slots). 
e.g. `ttl?=2` means that the tx will be rebroadcast over the next 5 slots (5 * 400ms).  

⚠️ users should take care to set either `max_order` ts or use atomic place/cancel/modify requests to prevent
double orders or orders being accepted later than intended.  

improving tx confirmation rates will require trial and error, try adjusting tx TTL and following parameters until
results are meet requirements:
- set `--extra-rpcs=<RPC_1>,<RPC_2>` to broadcast tx to multiple nodes
- set `--skip-tx-preflight` to disable preflight RPC checks
- setting a longer `ttl` per request
- set statically higher CU prices per request (see previous section) when no ack rates increase

**example request**

```bash
$ curl 'localhost:8080/v2/orders?ttl=2' -X POST \
-H 'content-type: application/json' \
-d # { order data ...}
```

## API Examples

Please refer to https://drift-labs.github.io/v2-teacher/ for further examples and reference documentation on various types, fields, and operations available on drift.

### HTTP API

### Get Markets

gets info on all available spot & perp markets

NB: spot marketIndex `0`/USDC is non-tradable

```bash
$ curl localhost:8080/v2/markets
```

**Response**

- `priceStep` smallest order price increment for the market
- `amountStep` smallest order amount increment for the market
- `minOrderSize` minimum order amount for the market
- `initialMarginRatio` collateral required to open position
- `maintenanceMarginRatio` collateral required to maintain position

```json
{
  "spot": [
    {
      "marketIndex": 1,
      "symbol": "SOL",
      "priceStep": "0.0001",
      "amountStep": "0.1",
      "minOrderSize": "0.1",
    }
    // ...
  ],
  "perp": [
    {
      "marketIndex": 0,
      "symbol": "SOL-PERP",
      "priceStep": "0.0001",
      "amountStep": "0.01",
      "minOrderSize": "0.01",
      "initialMarginRatio": "0.1",
      "maintenanceMarginRatio": "0.05"
    }
    // ...
  ]
}
```

## Get Margin Info
Returns the account margin requirements

```bash
$ curl localhost:8080/v2/user/marginInfo
```

**Response**

```json
{
  "initial": "141.414685",
  "maintenance": "132.522189"
}
```

## Get Leverage
Returns the account leverage

```bash
$ curl localhost:8080/v2/leverage
```

**Response**

```json
{
   "leverage" : "0.094489"
}
```

## Set Leverage
Set the max initial margin / leverage on sub-account

```bash
# max 2x
$ curl localhost:8080/v2/leverage -d '{"leverage":"2.0"}'
# max .1x
$ curl localhost:8080/v2/leverage -d '{"leverage":"0.1"}'
```

Returns solana tx signature on success

## Get Collateral
Returns the account's maintenance collateral

```bash
$ curl localhost:8080/v2/collateral
```

**Response**

```json
{
   "total":"1661.195815",
   "free":"1653.531255"
}
```

## Get Market Info

Returns market details (perps only)

```bash
$ curl localhost:8080/v2/marketInfo/0
```

**Response**

```json
{
  "openInterest": 662876,
  "maxOpenInterest": 850000
}
```

### Get Orderbook

To query or stream orderbooks via WebSocket, public DLOB servers are available at:

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
      "orderId": 35
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

### Get Position Info (perps only)

get extended position info for perps positions

```bash
# query info for perp market 0
$ curl localhost:8080/v2/positionInfo/0
```

note:
- `unrealizedPnL` is based on the oracle price at time of query
- `unsettledPnl` does not include unsettled funding amounts

**Response**

```json
{
  "amount": "-3.3",
  "averageEntry": "102.2629",
  "marketIndex": 0,
  "liquidationPrice": "213.405881",
  "unrealizedPnl": "-0.305832",
  "unsettledPnl": "2795.32259",
  "oraclePrice": "184.942200"
}
```

### Get Transaction Events

gets the transaction and parses events relevant to the provided user `subAccountId` (default will be used otherwise). Only events relevant to
the provided user will be returned.

```bash
# get events from tx hash 5JuobpnzPzwgdha4d7FpUHpvkinhyXCJhnPPkwRkdAJ1REnsJPK82q7C3vcMC4BhCQiABR4wfdbaa9StMDkCd9y5 for my subAccountId 0
$ curl localhost:8080/v2/transactionEvent/5JuobpnzPzwgdha4d7FpUHpvkinhyXCJhnPPkwRkdAJ1REnsJPK82q7C3vcMC4BhCQiABR4wfdbaa9StMDkCd9y5?subAccountId=0
```

A successful tx must bed confirm onchain and also execute successfully, the following table shows the possible responses from this endpoint:

|                   | execute ok                             | execute fail          |
|-------------------|------------------------------------------|-------------------------|
| confirm ok   | `200, {"success": true, "events": [...] }` | `200, { "success": false, "error": "msg" }` |
| confirm fail | `404`                                      | `404`


**Response**

A response with a fill belonging to sub-account 0

```json
{
  "events": [
    {
      "fill": {
        "side": "buy",
        "fee": "0.129744",
        "amount": "5",
        "price": "103.7945822",
        "oraclePrice": "102.386992",
        "orderId": 436,
        "marketIndex": 0,
        "marketType": "perp",
        "ts": 1708684880,
        "signature": "5JuobpnzPzwgdha4d7FpUHpvkinhyXCJhnPPkwRkdAJ1REnsJPK82q7C3vcMC4BhCQiABR4wfdbaa9StMDkCd9y5"
      }
    }
  ],
  "success":true
}
```

A response for a transaction not found. You should consider this transaction as dropped after around 5 seconds.

```json
{
  "code": 404,
  "reason": "tx not found: 4Mi32iRCqo2XXPjnV4bywyBpommVmbm5AN4wqbkgGFwDM3bTz6xjNfaomAnGJNFxicoMjX5x3D1b3DGW9xwkY7ms"
}
```

A response for a transaction that was found, but doesn't contain any events for the user

```json
{
  "events": [],
  "success":true
}
```

A response for a transaction that was confirmed onchain but failed execution e.g. due to exceeding slippage

```json
{
  "events": [],
  "error": "program error: 6015",
  "success": false
}
```
full list of error codes [here](https://drift-labs.github.io/v2-teacher/#errors)

### Get SOL balance
Return the on-chain SOL balance of the transaction signer (`DRIFT_GATEWAY_KEY`)
```bash
$ curl localhost:8080/v2/balance
```

```json
{ "balance": "0.12", "pubkey": "key" }
```

### Get Authority
Return the on-chain SOL balance of the transaction signer (`DRIFT_GATEWAY_KEY`)
```bash
$ curl localhost:8080/v2/authority
```

```json
{ "pubkey": "key" }
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
$ curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"marketIndex":1,"marketType":"spot"}'
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
            "reduceOnly": false
        }]
    }
}'
```

### Swap Orders

Executes a spot token swap using Jupiter routing and liquidity aggregation.
for more info see jup docs: https://dev.jup.ag/docs/api/swap-api/quote

⚠️ **Requires `JUPITER_API_KEY` environment variable to be set.** Get a free API key at [portal.jup.ag](https://portal.jup.ag). See [Jupiter API migration guide](https://dev.jup.ag/portal/migrate-from-lite-api) for details.

**Endpoint**: `POST /v2/swap`

**Parameters**:

Request body:
```json
{
  "inputMarket": number,   // Input spot market index
  "outputMarket": number,  // Output spot market index
  "exactIn": boolean,      // true = exactIn, false, exactOut 
  "amount": string,        // Amount of input token to sell when exactIn=true OR amount of output token to buy when exactIn=false
  "slippage": number,      // Max slippage in bps
  "useDirectRoutes": bool, // Direct Routes limits Jupiter routing to single hop routes only
  "excludeDexes": bool,    // comma separated list of dexes to exclude
}
```
dexes: https://api.jup.ag/swap/v1/program-id-to-label

**Response**:

Success (200):
```json
{
  "signature": string,   // Transaction signature
  "success": boolean    // Whether the swap was successful
}
```

Error (400/500):
```json
{
  "code": number,       // Error code
  "reason": string     // Error description
}
```

**Example**:
USDC to SOL
```bash
curl -X POST "http://localhost:8080/v2/swap" \
  -H "Content-Type: application/json" \
  -d '{
    "inputMarket": 0,
    "outputMarket": 1,
    "exactIn": true,
    "amount": "500.0",
    "slippageBps": 10
  }'
```

### Titan Swap Orders

Executes a spot token swap using Titan routing and liquidity aggregation.

⚠️ **Requires `TITAN_AUTH_TOKEN` environment variable to be set.**

**Endpoint**: `POST /v2/titan-swap`

**Environment Variables**:
| Variable | Description |
|----------|-------------|
| `TITAN_AUTH_TOKEN` | **Required**. Authentication token for Titan API |
| `TITAN_BASE_URL` | (Optional) Titan API base URL. Defaults to `https://api.titan.exchange` |

**Parameters**:

Request body:
```json
{
  "inputMarket": number,   // Input spot market index
  "outputMarket": number,  // Output spot market index
  "exactIn": boolean,      // true = exactIn, false = exactOut 
  "amount": string,        // Amount of input token to sell when exactIn=true OR amount of output token to buy when exactIn=false
  "slippageBps": number,   // Max slippage in bps
  "useDirectRoutes": bool, // (Optional) Direct Routes limits routing to single hop routes only
  "excludeDexes": string,  // (Optional) comma separated list of dexes to exclude
  "maxAccounts": number    // (Optional) limit number of accounts in swap instructions
}
```

**Response**:

Success (200):
```json
{
  "signature": string,   // Transaction signature
  "success": boolean    // Whether the swap was successful
}
```

Error (400/500):
```json
{
  "code": number,       // Error code
  "reason": string     // Error description
}
```

**Example**:
USDC to SOL
```bash
curl -X POST "http://localhost:8080/v2/titan-swap" \
  -H "Content-Type: application/json" \
  -d '{
    "inputMarket": 0,
    "outputMarket": 1,
    "exactIn": true,
    "amount": "500.0",
    "slippageBps": 10
  }'
```

## WebSocket API

Websocket API is provided for live event streams by default at port `127.0.0.1:1337`

### Subscribing

Subscribe to order and fills updates by a `subAccountId` (`0` is the drift default)

```ts
{"method":"subscribe", "subAccountId":0}
// unsubscribe
{"method":"unsubscribe", "subAccountId":0}
```

### Event Payloads

event payloads can be distinguished by "channel" field and the "data" payload is keyed by the event type

**order cancelled**

```json
{
  "data": {
    "orderCancel": {
      "orderId": 156,
      "ts": 1704777451,
      "signature": "2Cdo5Xgxj6uWY6dnWmuU5a8tH5fKC2K6YUqzVYKgnm8KkMVhPczBZrNEs4VGwEBMhgosifmNjBXSjFMWbGKJiqSz",
      "txIdx": 15
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
        "auctionDuration": 0
      },
      "ts": 1704777347,
      "signature": "2Cdo5Xgxj6uWY6dnWmuU5a8tH5fKC2K6YUqzVYKgnm8KkMVhPczBZrNEs4VGwEBMhgosifmNjBXSjFMWbGKJiqSz",
      "txIdx": 31
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
      "side": "sell",
      "fee": "-0.100549",
      "amount": "0.0326",
      "price": "61687",
      "oraclePrice": "61335.477737",
      "orderId": 11198929,
      "marketIndex": 1,
      "marketType": "perp",
      "ts": 1709248100,
      "txIdx": 12,
      "signature": "5xZvkv2Y5nGgpYpitFyzg99AVwqHPwspapjxBFmPygrKWdwPfaBd6Tm3sQEw3k8GsZAd68cJ9cPr89wJ11agWthp",
      "maker": "B24N44F45nq4Sk2gVQqtWG3bfXW2FJKZrVqhhWcxJNv3",
      "makerOrderId": 11198929,
      "makerFee": "-0.100549",
      "taker": "Fii4Aio6rGoa8BDH6mR7JfTWA73FA7No1SNauYEWCoVn",
      "takerOrderId": 40,
      "takerFee": "0.502750"
    }
  },
  "channel": "fills",
  "subAccountId": 1
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

**funding payment**

settled funding payment event for open perp positions

- `amount` is usdc

```json
{
  "data": {
    "fundingPayment": {
      "amount": "0.005558",
      "marketIndex": 0,
      "ts": 1708664443,
      "signature": "2Cdo5Xgxj6uWY6dnWmuU5a8tH5fKC2K6YUqzVYKgnm8KkMVhPczBZrNEs4VGwEBMhgosifmNjBXSjFMWbGKJiqSz",
      "txIdx": 1
    }
  },
  "channel": "funding",
  "subAccountId": 0
}
```

**jupiter swap**
denotes a subaccount swapped some amount of token (in) for another token (out)

- `amountIn`: token amount sent at swap start
- `marketIn`: market index of token sent
- `amountOut`: token amount received at swap end
- `marketOut`: market index of token received

```json
{
  "data": {
    "swap": {
      "amountIn": "2.000000",
      "amountOut": "0.013732454",
      "marketIn": 0,
      "marketOut": 1,
      "ts": 1746418907,
      "txIdx": 104,
      "signature": "2mwW6gbPNWzgUWqVCJKCM55HWCTDG5iZUwjU1wVLdKgBzQz7yQj4ZsePQEpsECdcXJ1mJCGVHEvzCVf9mbhHnN7u"
    }
  },
  "channel": "swap",
  "subAccountId": 0
}
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

### FAQ

#### `AccountNotFound`

Usually means the drift user (sub)account has not been initialized.
Use the UI or Ts/Python sdk to initialize the sub-account first.

```json
{
  "code": 500,
  "reason": "AccountNotFound: pubkey=FQHZg9yU2o5uN9ERQyeTNNAMe3JWf13gce2DUj6x2HTv"
}
```

#### `429`s / gateway hitting RPC rate limits

this can occur during gateway startup as drift market data is pulled from the network and subscriptions are initialized.  
try setting `INIT_RPC_THROTTLE=2` for e.g. 2s or longer, this allows some time between request bursts on start up.  

The free \_api.mainnet-beta.solana.com_ RPC support is limited due to rate-limits

```rust
Some(GetProgramAccounts), kind: Reqwest(reqwest::Error { kind: Status(429), ...
```

#### Slow queries

Queries longer than a few _ms_ may be due to missing market subscriptions.  
Ensure gateway is properly configured with intended markets. 
