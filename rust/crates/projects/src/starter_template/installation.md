Replace this page with real installation instructions for your product. It
demonstrates the two building blocks most install guides need: tabs for
package managers and steps for a walkthrough.

## Install the package

<Tabs>

<Tab title="npm">

```bash
npm install @acme/sdk
```

</Tab>

<Tab title="pnpm">

```bash
pnpm add @acme/sdk
```

</Tab>

<Tab title="yarn">

```bash
yarn add @acme/sdk
```

</Tab>

</Tabs>

## Set up your project

<Steps>

<Step title="Grab an API key">

Create a key in your product's dashboard and keep it secret — treat it like a
password.

</Step>

<Step title="Configure the client">

```ts title="acme.ts"
import { Acme } from '@acme/sdk';

export const acme = new Acme({ apiKey: process.env.ACME_API_KEY });
```

</Step>

<Step title="Make your first call">

```ts
const widget = await acme.widgets.create({ name: 'My first widget' });
console.log(widget.id);
```

</Step>

</Steps>

<Tip>

Swap `@acme/sdk` for your real package name. Code blocks support syntax
highlighting for dozens of languages, plus a `title="file.ts"` header.

</Tip>

Next: document your API in the [API reference](/api-reference/create-a-widget).
