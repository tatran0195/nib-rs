Show readers how to authenticate against your API. This example uses a bearer
token; adapt it to your scheme.

All requests must include your API key in the `Authorization` header:

<CodeGroup>

```bash title="cURL"
curl https://api.example.com/v1/widgets \
  -H "Authorization: Bearer $ACME_API_KEY"
```

```ts title="TypeScript"
const res = await fetch('https://api.example.com/v1/widgets', {
  headers: { Authorization: `Bearer ${process.env.ACME_API_KEY}` },
});
```

```python title="Python"
import requests

res = requests.get(
    "https://api.example.com/v1/widgets",
    headers={"Authorization": f"Bearer {API_KEY}"},
)
```

</CodeGroup>

<Warning>

Never ship API keys in client-side code. Call your API from a server and keep
keys in environment variables.

</Warning>
