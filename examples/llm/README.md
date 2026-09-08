# LLM pipeline

A chat-completions client written in Kira: a question goes out over verified
TLS, the answer comes back as JSON, and the conversation remembers what was
said.

```sh
cargo build -p kira-network

# Against a real service.
OPENROUTER_API_KEY=... kira run --backend llvm examples/llm

# With no key, against the loopback service `kira-network` ships.
kira run --backend vm examples/llm
```

Both run the same code. Only the base URL differs, and nothing in `chat.kira`
knows which of the two it is talking to — which is what makes the offline run
worth having: the pipeline is exercised end to end on a machine with no key and
no internet.

`OPENROUTER_MODEL` chooses the model (default `openai/gpt-5-nano`) and
`OPENROUTER_BASE_URL` the service, so pointing this at anything that speaks the
same shape needs no edit. A key with no billing attached reaches the `:free`
models, which this drives unchanged:

```sh
OPENROUTER_API_KEY=... OPENROUTER_MODEL=minimax/minimax-m3:free \
    kira run --backend llvm examples/llm
```

```
llm: minimax/minimax-m3:free at https://openrouter.ai/api/v1
> Name one thing a compiler does. Answer in five words or fewer.
Translates source code to machine code.
  [minimax/minimax-m3:free stop 182+9 tokens]
> Say that again in French.
Traduit le code source en code machine.
  [minimax/minimax-m3:free stop 207+10 tokens]
history 5
0
```

## What is in it

| File | Holds |
| --- | --- |
| `app/http.kira` | The `@FFI.Extern` declarations and one `httpSend`: a request goes out, an answer comes back, and the Kira scheduler keeps running while Tokio drives the socket. |
| `app/chat.kira` | Messages in, a reply out: the request document, the response document, the five ways it can fail, the retry policy, and a `Conversation` that keeps its own history. |
| `app/main.kira` | Reads the environment, takes two turns, prints what came back. |

## The request

Built as a `JsonValue` and written with `writeJson`, never by pasting strings
together. A message containing a quote or a newline is ordinary — it is the
user's text — and a hand-built body would produce a document the service
refuses, or worse, a different one than the user typed.

## Failure

`ChatFailure` names five, because there are five different things to do about
them.

| Variant | Means | Retried |
| --- | --- | --- |
| `Transport(code)` | The service was never reached: the network library's own negative code. | A deadline, a refused connection, a broken read, a failed handshake, a name that did not resolve. Not a URL that cannot be parsed. |
| `Status(status)` | An HTTP status with no error document to explain it. | 408, 429, and anything 5xx. |
| `Service(message)` | The service's own error text — the one worth showing a user. | No. It answered, and it will answer the same way. |
| `Malformed(detail)` | A body that is not the document this expects, with the byte offset. | No. |
| `Truncated(budget)` | The answer ran out of room before it wrote anything. | No — the same budget stops in the same place. |

The wait doubles between sends, so a service that is briefly busy is given room
without a program hammering it.

`Truncated` is why `max_tokens` is 2000 rather than the 200 a one-sentence
answer needs. A model that reasons before it writes spends the budget on
thinking first: `openai/gpt-5-nano` answered this example's second question with
1567 completion tokens, nearly all of them reasoning. At 200 it returned an
empty string with `finish_reason: length`, which is a truncation rather than an
answer, and saying so is the difference between a caller raising the budget and
a caller looking for the bug in its own parsing.

A turn that failed does not stay in the history. Leaving the question in would
ask the service, on the next turn, to answer something it already refused.

## The second question

`Say that again in French.` means nothing on its own. It is the second turn
because answering it requires the first one, so a history that was not carried
shows up as an answer that makes no sense — or, against the loopback service, as
a request whose token count did not grow.

## Trying it without a key

The loopback service answers with the request it was given, so the offline run
checks that this turn's question crossed and that the earlier turns went with
it:

```
llm: no OPENROUTER_API_KEY, running against the loopback service
turn 1: carried=true history=true tokens=239+119
turn 2: carried=true history=true tokens=592+296
history 5
0
```

The prompt grows from 239 tokens to 592 between the turns because the second
request carries the system message, both questions, and the first answer.
`history 5` is those five messages. The last line is the number of turns that
failed.
