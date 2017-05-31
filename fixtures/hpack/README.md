# hpack-test-case

## Spec

HPACK: Header Compression for HTTP/2
https://tools.ietf.org/html/rfc7541


## Directory

The ```raw-data``` directory has original stories of header data in
json.

Other than ```raw-data``` directory, the HPACK implementations have
their own directories to store the result of its encoder.

You can perform interoperability testing for your implementation with
them.

## File Name

Each json in story-#{n}.json is story case and shares compression
context. Each story is either series of requests or responses.

## JSON Format

Each json has:

- description: general description of encoding strategy or implementation.
- cases: array of test cases.
  - seqno: a sequence number. 0 origin.
  - header_table_size: the header table size sent in SETTINGS_HEADER_TABLE_SIZE and ACKed just before this case. The first case should contain this field. If omitted, the default value, 4,096, is used.
  - wire: encoded wire data in hex string.
  - headers: decoded http header in hash.

To test the decoder implementation, for each story file, for each case
in ```cases``` in the order they appear, decode compressed header
block in ```wire``` and verify the result against the header set in
```headers```. Please note that elements in ```cases``` share the same
compression context.

To test the encoder implementation, generate json story files by
encoding header sets in ```headers```. Using json files in
```raw-data``` is handy. Then use your decoder to verify that it can
successfully decode the compressed header block. If you can play with
other HPACK decoder implementations, try decoding your encoded data
with them. If there is any mismatch, then there must be a bug in
somewhere either encoder or decoder, or both.

```js
{
  "description": "Encoded request headers with Literal without index only.",
  "cases": [
    {
      "seqno": 0,
      "header_table_size": 4096,
      "wire": "1234567890abcdef",
      "headers": [
        { ":method": "GET" },
        { ":scheme": "http" },
        { ":authority": "example.com" },
        { ":path": "/" },
        { "x-my-header": "value1,value2" }
      ]
    },
    .....
  ]
}
```

## Original Data

These Header Data are converted from https://github.com/http2/http_samples


## Compression Ratio

https://github.com/http2jp/hpack-test-case/wiki/Compression-Ratio


## Contributor

- jxck
- summerwind
- kazu-yamamoto
- tatsuhiro-t


## License

The MIT License (MIT)
