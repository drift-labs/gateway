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

### Get Market Info
```bash
~: curl localhost:8080/v2/markets
```

### Get Orders
```bash
~: curl localhost:8080/v2/orders
```

### Get Positions
```bash
~: curl localhost:8080/v2/positions
```

### Place Orders
```bash
~: curl localhost:8080/v2/orders -X POST -H 'content-type: application/json' -d '{
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
~: curl localhost:8080/v2/orders -X PATCH -H 'content-type: application/json' -d '{
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
~: curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"market":{"id":1,"type":"perp"}}'
# cancel by order ids
~: curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"ids":[1,2,3,4]}'
# cancel by user assigned order ids
~: curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json' -d '{"userIds":[1,2,3,4]}'
# cancel all
~: curl localhost:8080/v2/orders -X DELETE -H 'content-type: application/json'
```

### Stream Orderbook
```bash
~: curl localhost:8080/v2/orderbooks -N -X GET -H 'content-type: application/json' -d '{"market":{"id":3,"type":"perp"}}'
```


# TODO:
- return error status for failed txs?
- allow empty json body on queries