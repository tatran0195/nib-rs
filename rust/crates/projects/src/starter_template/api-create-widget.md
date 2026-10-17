A worked example of an endpoint page. Duplicate it for each endpoint in your
API, or delete the group if you do not document an API.

## POST /v1/widgets

Creates a widget and returns it.

### Body parameters

<ParamField body="name" type="string" required="true">

A human-readable name, 1-80 characters.

</ParamField>

<ParamField body="color" type="string" default="terracotta">

Any CSS color keyword or hex value.

</ParamField>

<ParamField body="tags" type="string[]">

Up to 10 tags used for filtering in list endpoints.

</ParamField>

### Response

<ResponseField name="id" type="string" required="true">

Unique identifier, prefixed with `wid_`.

</ResponseField>

<ResponseField name="name" type="string" required="true">

The name you supplied.

</ResponseField>

<ResponseField name="createdAt" type="string">

ISO 8601 creation timestamp.

</ResponseField>

### Example

<CodeGroup>

```bash title="Request"
curl -X POST https://api.example.com/v1/widgets \
  -H "Authorization: Bearer $ACME_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{ "name": "My first widget" }'
```

```json title="Response"
{
  "id": "wid_1a2b3c",
  "name": "My first widget",
  "createdAt": "2026-07-10T12:00:00Z"
}
```

</CodeGroup>

See [Authentication](/api-reference/authentication) for how to get a key.
