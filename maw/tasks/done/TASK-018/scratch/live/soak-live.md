# TASK-018 soak report

mode: live (real bot); duration 155.6 s; 75 Telegram calls; 2 hub runs

| operation | Bot API method | accepted |
|---|---|---|
| send | sendMessage (replies, answers, notices, separators, blocks) | 5 |
| permission | sendMessage (permission prompts) | 2 |
| stream | sendMessage (transcript stream) | 26 |
| document | sendDocument | 0 |
| edit | editMessageText | 3 |
| react | setMessageReaction | 0 |
| callback | answerCallbackQuery | 0 |
| create_topic | createForumTopic | 3 |
| edit_topic | editForumTopic | 13 |
| delete | deleteMessage (service messages) | 13 |

- topics: 3 (A, A #2, B); separators: 1; service messages: 13 shown, 13 deleted, 0 left
- 429: 3 (retry_after 1s), each followed by a pause of the whole queue and one retry; other errors: 0; answered locally (live: reactions and callback answers on simulated ids): 7
- permission latency (request written to the agent -> sendMessage): A 4011 ms (other topic), A #2 854 ms (own topic behind its stream); A #2 burst lines written before the request and sent after the prompt: 6
- stream: 33 lines in 26 messages; metered sends peak at 81% of the bucket in any 1, 3 or 60 s window; smallest gap 1000 ms (bucket 5 + 1 per 4000 ms, min gap 1000 ms)
- edits and topic calls are counted separately and are not compared with the 20 messages/min group limit: Telegram publishes no number for them
- burst: 16 lines in topic A #2, 4 in B; 2 of A #2 sent before the prompts were asked
- registry.json: 3 slots (A: a5a5a5a5, A #2: a2a2a2a2, B: b1b1b1b1), nested ee0e0e0e -> parent a1a1a1a1
- pending updates of the bot consumed at the start (written before the run, not seen by the stopped hub; at most 100 counted): 0
