# Turn efficiency candidates

Generated 2026-10-08T10:19:52.010Z from native session files (cohort and usage via baseline.mjs). Window N=3 model responses; short reply ≤ 300 chars. Window … to 2026-10-08T09:59:18.000Z (inclusive) on subjects and associated requests. Counts are candidates selected by tool names and message wrappers; associated requests and costs are upper bounds, not avoidable cost.

## Cohort

| Runtime | Sessions | Requests | Turns | Partial turns at cutoff | Tool calls | Crew tool calls | Codex exec wrappers | Agents |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| claude | 49 | 18,591 | 2,030 | 11 | 19,009 | 3,842 | 0 | architect, auditor, baseline, content, designer, fix-core, fix-runtime, graph, integrator, lead, linux, native-architect, orchestrator, qa, rev-tui, reviewer, sandbox, scheduler, tui, ui, unknown, vcs, vcs-batchtests, vcs-fallback, vcs-hubtests, vcs-mismatch, vcs-pin, vcs-pinfix, vcs-pinfix2, vcs-wakeups, web |
| codex | 45 | 10,452 | 3,692 | 7 | 19,581 | 10,358 | 8,225 | architect, cache-reviewer, content, designer, graph, native-architect, native-transport, qa, reviewer, runtime, sandbox, scheduler, ui, vcs-pinfix2, web |

agent_inbox result kinds: claude messages=108, id_not_found=28, messages_persisted=1, empty=1, error=9; codex messages=1552, id_not_found=22, empty=945, error=5.

Non-message inbox result strings (normalized): `No new messages.` ×946, `no message 'm-X' sent or received by you` ×49, `incompatible crew protocol: client speaks protocol N, daemon speaks N; restart t` ×13, `<persisted-output>` ×1, `Error: result (N,N characters across N lines) exceeds maximum allowed toke` ×1, `no message 'w-X' sent or received by you` ×1.

## inbox_empty

candidate agent_inbox calls whose result says no new messages; associated request = the next request when the call was alone (upper bound), else result bytes only.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 946 | 29 | 14 | 344 | 94,639,962 | 94,452,096 | 0 | 187,866 | 39,487 | no price (+344 req no price) | 15,136 |
| runtime codex | 945 | 28 | 13 | 344 | 94,639,962 | 94,452,096 | 0 | 187,866 | 39,487 | no price (+344 req no price) | 15,120 |
| runtime claude | 1 | 1 | 1 | 0 | — | — | — | — | — | — | 16 |
| kind bundled | 599 | 29 | 14 | 0 | — | — | — | — | — | — | 9,584 |
| kind alone | 344 | 20 | 11 | 344 | 94,639,962 | 94,452,096 | 0 | 187,866 | 39,487 | no price (+344 req no price) | 5,504 |
| kind no_consumer | 3 | 3 | 3 | 0 | — | — | — | — | — | — | 48 |

Cost basis (total row): 344 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

Top agents: codex:content 245, codex:graph 134, codex:native-architect 108, codex:runtime 98, codex:qa 96, codex:scheduler 89, codex:ui 45, codex:native-transport 44.

## inbox_after_delivery

agent_inbox calls within N model responses after an in-band delivery whose messages all arrived in full; candidate = empty or duplicate-only result.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 675 | 37 | 15 | 387 | 103,353,995 | 103,089,766 | 78,860 | 185,369 | 12,697 | $1.75 (+377 req no price) | 638,750 |
| runtime codex | 647 | 25 | 11 | 377 | 100,612,739 | 100,427,392 | 0 | 185,347 | 8,957 | no price (+377 req no price) | 208,608 |
| runtime claude | 28 | 12 | 10 | 10 | 2,741,256 | 2,662,374 | 78,860 | 22 | 3,740 | $1.75 | 430,142 |
| kind empty alone | 317 | 14 | 9 | 317 | 87,064,000 | 86,915,584 | 0 | 148,416 | 4,755 | no price (+317 req no price) | 5,072 |
| kind empty bundled | 168 | 19 | 10 | 0 | — | — | — | — | — | — | 2,688 |
| kind duplicate_only bundled | 118 | 30 | 14 | 0 | — | — | — | — | — | — | 377,593 |
| kind duplicate_only alone | 71 | 16 | 12 | 70 | 16,289,995 | 16,174,182 | 78,860 | 36,953 | 7,942 | $1.75 (+60 req no price) | 253,381 |
| kind empty no_consumer | 1 | 1 | 1 | 0 | — | — | — | — | — | — | 16 |

Cost basis (total row): 377 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

All inbox calls inside a post-delivery window: 1123 (new_messages alone 150, id_not_found bundled 28, duplicate_only bundled 118, duplicate_only alone 71, new_messages bundled 256, id_not_found alone 8, error alone 3, error bundled 3, empty alone 317, empty bundled 168, empty no_consumer 1).

Top agents: codex:content 204, codex:native-architect 92, codex:qa 79, codex:graph 73, codex:runtime 49, codex:designer 37, codex:native-transport 30, codex:ui 26.

## inbox_duplicate_only

candidate agent_inbox calls whose returned message ids were all delivered in full in-band earlier in the session.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 231 | 40 | 20 | 81 | 18,643,322 | 18,490,671 | 113,920 | 38,731 | 11,291 | $2.61 (+61 req no price) | 859,372 |
| runtime codex | 185 | 22 | 11 | 61 | 13,663,649 | 13,624,960 | 0 | 38,689 | 4,399 | no price (+61 req no price) | 270,973 |
| runtime claude | 46 | 18 | 16 | 20 | 4,979,673 | 4,865,711 | 113,920 | 42 | 6,892 | $2.61 | 588,399 |
| kind bundled | 149 | 37 | 18 | 0 | — | — | — | — | — | — | 518,044 |
| kind alone | 82 | 22 | 16 | 81 | 18,643,322 | 18,490,671 | 113,920 | 38,731 | 11,291 | $2.61 (+61 req no price) | 341,328 |

Cost basis (total row): 61 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

Top agents: codex:qa 56, codex:content 29, codex:designer 26, codex:reviewer 19, codex:graph 18, codex:cache-reviewer 11, claude:reviewer 9, codex:runtime 9.

## repeated_read

candidate read tool calls (tasks_list, task_get, agents_list, git_status, git_diff, usage_status, agent_inbox by id) byte-identical to the previous same tool+args call with no own state change between.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 9 | 7 | 6 | 0 | — | — | — | — | — | — | 196,237 |
| runtime codex | 7 | 5 | 4 | 0 | — | — | — | — | — | — | 188,708 |
| runtime claude | 2 | 2 | 2 | 0 | — | — | — | — | — | — | 7,529 |
| kind task_get bundled | 6 | 6 | 5 | 0 | — | — | — | — | — | — | 16,874 |
| kind tasks_list bundled | 2 | 2 | 2 | 0 | — | — | — | — | — | — | 179,243 |
| kind git_status bundled | 1 | 1 | 1 | 0 | — | — | — | — | — | — | 120 |

Cost basis (total row): all associated requests priced exactly.

Top agents: codex:graph 4, claude:unknown 1, claude:scheduler 1, codex:runtime 1, codex:native-architect 1, codex:qa 1.

## status_only_turn

candidate turns whose only tool calls are agent_status / task_update / agent_send type=status, or with no tool calls and a short reply; associated = every request in the turn.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 1,329 | 73 | 26 | 1,998 | 572,813,882 | 547,308,170 | 22,481,041 | 3,024,671 | 156,378 | $304.64 (+1,386 req no price) | 0 |
| runtime codex | 819 | 36 | 11 | 1,386 | 327,505,545 | 324,482,944 | 0 | 3,022,601 | 91,282 | no price (+1,386 req no price) | 0 |
| runtime claude | 510 | 37 | 23 | 612 | 245,308,337 | 222,825,226 | 22,481,041 | 2,070 | 65,096 | $304.64 | 0 |
| kind message | 933 | 46 | 20 | 1,465 | 409,375,138 | 395,618,834 | 11,700,004 | 2,056,300 | 111,079 | $182.44 (+1,172 req no price) | 0 |
| kind notice | 263 | 70 | 26 | 364 | 100,378,408 | 93,398,761 | 6,142,470 | 837,177 | 24,519 | $72.57 (+190 req no price) | 0 |
| kind non_crew | 75 | 10 | 10 | 98 | 40,465,993 | 36,705,574 | 3,760,089 | 330 | 12,369 | $28.27 | 0 |
| kind batch | 58 | 20 | 14 | 71 | 22,594,343 | 21,585,001 | 878,478 | 130,864 | 8,411 | $21.35 (+24 req no price) | 0 |

Cost basis (total row): 1,386 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

Top agents: codex:qa 168, codex:designer 149, claude:reviewer 126, codex:content 118, codex:runtime 78, codex:graph 75, claude:orchestrator 71, codex:native-architect 71.

## status_or_read_only_turn

candidate turns (not already status-only) whose tool calls are only status calls plus crew reads (agent_inbox, tasks_list, task_get, agents_list).

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 525 | 38 | 19 | 1,090 | 277,725,948 | 275,089,415 | 605,512 | 2,031,021 | 92,367 | $10.59 (+1,005 req no price) | 0 |
| runtime codex | 489 | 26 | 11 | 1,005 | 254,009,799 | 251,979,008 | 0 | 2,030,791 | 77,883 | no price (+1,005 req no price) | 0 |
| runtime claude | 36 | 12 | 10 | 85 | 23,716,149 | 23,110,407 | 605,512 | 230 | 14,484 | $10.59 | 0 |
| kind message | 442 | 29 | 14 | 909 | 231,217,016 | 229,911,968 | 24,637 | 1,280,411 | 69,320 | $1.45 (+892 req no price) | 0 |
| kind notice | 63 | 24 | 13 | 139 | 33,952,308 | 33,448,083 | 166,249 | 337,976 | 16,256 | $4.46 (+85 req no price) | 0 |
| kind batch | 15 | 6 | 6 | 30 | 7,803,337 | 7,389,522 | 1,215 | 412,600 | 4,678 | $0.13 (+28 req no price) | 0 |
| kind non_crew | 5 | 3 | 3 | 12 | 4,753,287 | 4,339,842 | 413,411 | 34 | 2,113 | $4.54 | 0 |

Cost basis (total row): 1,005 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

Top agents: codex:content 175, codex:native-architect 70, codex:graph 52, codex:qa 48, codex:runtime 38, codex:designer 37, codex:ui 36, claude:unknown 19.

## notice_turn

turns started by crew notices or status messages; candidate = the turn was status-only or status/read-only.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 530 | 73 | 26 | 872 | 239,788,064 | 228,980,767 | 8,576,256 | 2,231,041 | 74,544 | $111.14 (+609 req no price) | 0 |
| runtime codex | 335 | 36 | 11 | 609 | 146,486,531 | 144,256,256 | 0 | 2,230,275 | 45,084 | no price (+609 req no price) | 0 |
| runtime claude | 195 | 37 | 23 | 263 | 93,301,533 | 84,724,511 | 8,576,256 | 766 | 29,460 | $111.14 | 0 |
| kind message:status | 200 | 32 | 15 | 362 | 104,385,449 | 101,067,062 | 2,265,901 | 1,052,486 | 32,401 | $33.93 (+330 req no price) | 0 |
| kind notice:no_tool_call | 89 | 25 | 11 | 181 | 44,996,906 | 44,636,288 | 0 | 360,618 | 11,815 | no price (+181 req no price) | 0 |
| kind notice:fast_forward | 74 | 16 | 12 | 96 | 40,758,644 | 36,316,904 | 4,192,446 | 249,294 | 8,638 | $44.78 (+24 req no price) | 0 |
| kind notice:online_idle | 57 | 57 | 23 | 57 | 2,014,462 | 820,162 | 651,371 | 542,929 | 770 | $7.10 (+30 req no price) | 0 |
| kind notice:unresponsive | 37 | 2 | 2 | 45 | 11,695,795 | 11,678,095 | 17,606 | 94 | 3,416 | $2.48 | 0 |
| kind notice:team_messages_came | 17 | 9 | 8 | 33 | 13,389,442 | 12,065,012 | 1,324,326 | 104 | 4,380 | $18.87 | 0 |
| kind notice:task_reminder | 14 | 7 | 7 | 28 | 9,894,059 | 9,878,719 | 11,332 | 4,008 | 3,693 | $1.63 (+9 req no price) | 0 |
| kind notice:task_event | 13 | 3 | 2 | 18 | 3,558,372 | 3,457,811 | 100,511 | 50 | 3,015 | $1.50 | 0 |
| kind notice:compacted | 9 | 8 | 8 | 17 | 1,208,286 | 1,195,876 | 2,213 | 10,197 | 1,042 | $0.04 (+15 req no price) | 0 |
| kind notice:card_revised | 8 | 7 | 6 | 16 | 4,011,915 | 4,004,096 | 0 | 7,819 | 1,143 | no price (+16 req no price) | 0 |
| kind notice:refusal | 6 | 3 | 3 | 10 | 2,372,599 | 2,364,012 | 8,555 | 32 | 2,651 | $0.54 | 0 |
| kind batch:status | 4 | 2 | 2 | 7 | 1,071,899 | 1,066,861 | 1,636 | 3,402 | 1,368 | $0.17 (+4 req no price) | 0 |

Cost basis (total row): 609 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

All notice/status-started turns: 1380.

| Input class | Turns | Candidates (status/read only) | Status-only | Associated requests (upper bound) | Input total | Cache read | Associated list cost (input side, upper bound) |
|---|---:|---:|---:|---:|---:|---:|---:|
| message:status | 480 | 200 | 131 | 1,156 | 334,081,894 | 323,855,628 | $72.05 (+879 req no price) |
| notice:no_tool_call | 162 | 89 | 60 | 240 | 60,717,708 | 60,303,932 | $1.70 (+228 req no price) |
| notice:fast_forward | 159 | 74 | 72 | 568 | 208,847,050 | 202,874,367 | $86.01 (+167 req no price) |
| notice:usage_lifted | 142 | 0 | 0 | 916 | 271,325,242 | 264,932,736 | no price (+916 req no price) |
| notice:task_reminder | 72 | 14 | 10 | 919 | 336,401,414 | 332,741,146 | $73.40 (+237 req no price) |
| notice:team_messages_came | 60 | 17 | 13 | 473 | 188,056,732 | 182,875,220 | $104.84 |
| notice:online_idle | 57 | 57 | 57 | 57 | 2,014,462 | 820,162 | $7.10 (+30 req no price) |
| notice:unresponsive | 57 | 37 | 30 | 106 | 27,946,947 | 27,886,392 | $6.06 |
| notice:task_event | 51 | 13 | 8 | 105 | 24,346,934 | 24,059,938 | $7.11 |
| notice:compacted | 43 | 9 | 6 | 1,163 | 204,625,141 | 201,319,488 | $72.61 (+170 req no price) |
| batch:status | 40 | 4 | 1 | 145 | 28,926,664 | 28,643,121 | $7.35 (+13 req no price) |
| notice:card_revised | 28 | 8 | 2 | 52 | 11,735,728 | 11,633,536 | no price (+52 req no price) |
| notice:refusal | 17 | 6 | 3 | 44 | 7,061,299 | 7,020,176 | $1.73 |
| notice:usage_limit | 6 | 2 | 2 | 16 | 3,222,058 | 3,201,963 | $0.80 |
| notice:restarted | 3 | 0 | 0 | 30 | 14,453,531 | 13,115,392 | no price (+30 req no price) |
| notice:model_restored | 2 | 0 | 0 | 10 | 4,319,536 | 3,445,167 | $18.35 |
| notice:all_done | 1 | 0 | 0 | 4 | 984,509 | 981,547 | $0.22 |

Turn-starting status messages by sender (scheduler = daemon notices and the scheduler member; a teammate status starts a turn only when the recipient was waiting):

| Sender | Turns | Candidates (status/read only) | Status-only | Associated requests (all) | Input total (all) | Associated list cost (all) | Associated requests (candidates) | Associated list cost (candidates) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| qa | 107 | 47 | 25 | 167 | 41,119,846 | $0.25 (+162 req no price) | 86 | no price (+86 req no price) |
| orchestrator | 77 | 24 | 14 | 105 | 34,731,001 | $0.61 (+101 req no price) | 43 | no price (+43 req no price) |
| scheduler | 60 | 23 | 17 | 158 | 62,114,868 | $34.06 (+56 req no price) | 41 | $11.77 (+34 req no price) |
| graph | 45 | 21 | 14 | 148 | 28,220,872 | $8.48 (+59 req no price) | 42 | $5.70 (+37 req no price) |
| content | 37 | 13 | 11 | 78 | 27,171,956 | $1.14 (+70 req no price) | 24 | $0.29 (+22 req no price) |
| runtime | 28 | 18 | 12 | 153 | 37,719,970 | $9.62 (+113 req no price) | 34 | no price (+34 req no price) |
| designer | 27 | 9 | 8 | 47 | 13,935,819 | $0.05 (+46 req no price) | 15 | $0.05 (+14 req no price) |
| native-transport | 24 | 9 | 2 | 29 | 9,327,445 | no price (+29 req no price) | 18 | no price (+18 req no price) |
| ui | 22 | 15 | 12 | 197 | 52,130,483 | $0.34 (+193 req no price) | 24 | $0.34 (+20 req no price) |
| native-architect | 19 | 2 | 1 | 8 | 2,265,421 | no price (+8 req no price) | 4 | no price (+4 req no price) |
| reviewer | 15 | 6 | 3 | 32 | 8,461,976 | $0.34 (+30 req no price) | 9 | $0.16 (+8 req no price) |
| cache-reviewer | 6 | 4 | 3 | 10 | 4,441,091 | $2.05 (+4 req no price) | 8 | $2.05 (+2 req no price) |
| vcs-pinfix2 | 4 | 3 | 3 | 10 | 6,142,967 | $1.11 (+4 req no price) | 5 | $0.18 (+4 req no price) |
| web | 3 | 3 | 3 | 5 | 1,805,653 | $10.06 (+2 req no price) | 5 | $10.06 (+2 req no price) |
| integrator | 3 | 1 | 1 | 5 | 1,458,218 | $0.25 (+2 req no price) | 2 | no price (+2 req no price) |
| sandbox | 1 | 1 | 1 | 1 | 422,424 | $3.16 | 1 | $3.16 |
| vcs-pin | 1 | 1 | 1 | 1 | 870,208 | $0.18 | 1 | $0.18 |
| vcs-fallback | 1 | 0 | 0 | 2 | 1,741,676 | $0.35 | 0 | — |

Top agents: codex:content 75, codex:qa 57, claude:unknown 53, codex:designer 49, codex:runtime 30, codex:native-architect 30, claude:reviewer 27, codex:graph 25.

## delivery_round_trips

turns started by a delivered crew message: tool calls by category; associated = requests that consumed the crew read calls (split among sibling calls). 13 of these turns were still open at the cutoff (partial).

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 4,773 | 60 | 29 | 1,433.83 | 361,997,834 | 356,982,520 | 780,458 | 4,234,857 | 1,195,692 | $30.16 (+1,113.72 req no price) | 5,740,847 |
| runtime codex | 3,419 | 28 | 11 | 1,113.72 | 270,163,417 | 265,929,218 | 0 | 4,234,200 | 1,024,181 | no price (+1,113.72 req no price) | 3,937,189 |
| runtime claude | 1,354 | 32 | 26 | 320.11 | 91,834,417 | 91,053,302 | 780,458 | 657 | 171,510 | $30.16 | 1,803,658 |
| kind message:answer | 1,054 | 41 | 18 | 261.79 | 76,716,931 | 76,049,309 | 44,394 | 623,228 | 243,891 | $1.29 (+251.96 req no price) | 732,571 |
| kind message:artifact | 772 | 42 | 19 | 144.06 | 37,148,761 | 36,594,405 | 22,220 | 532,135 | 100,927 | $0.95 (+133.48 req no price) | 400,081 |
| kind message:message | 613 | 47 | 20 | 173.35 | 39,050,850 | 37,752,716 | 43,016 | 1,255,118 | 144,187 | $1.60 (+155.27 req no price) | 843,582 |
| kind message:status | 480 | 38 | 16 | 134.15 | 35,893,880 | 35,569,560 | 41,688 | 282,632 | 145,222 | $0.98 (+122.65 req no price) | 422,845 |
| kind message:question | 362 | 34 | 14 | 95.92 | 27,725,079 | 27,428,089 | 13,609 | 283,382 | 143,757 | $0.92 (+88.42 req no price) | 341,594 |
| kind message:review_result | 300 | 46 | 20 | 50.26 | 13,311,967 | 13,031,930 | 10,915 | 269,122 | 32,733 | $0.44 (+42.76 req no price) | 117,802 |
| kind batch | 286 | 40 | 18 | 120.73 | 29,015,287 | 28,667,826 | 58,655 | 288,806 | 112,258 | $3.81 (+88.06 req no price) | 574,907 |
| kind notice:no_tool_call | 162 | 27 | 11 | 22.92 | 5,510,356 | 5,460,757 | 0 | 49,599 | 5,406 | no price (+22.92 req no price) | 96,029 |
| kind notice:fast_forward | 159 | 22 | 14 | 24.83 | 5,629,045 | 5,549,667 | 32,266 | 47,112 | 14,523 | $0.89 (+15.83 req no price) | 124,523 |
| kind message:review_request | 148 | 9 | 5 | 86.27 | 23,794,919 | 23,575,175 | 85,839 | 133,905 | 48,602 | $4.99 (+47.42 req no price) | 437,057 |
| kind message:task | 132 | 49 | 24 | 198.98 | 44,891,569 | 44,367,938 | 121,998 | 401,632 | 142,895 | $6.16 (+117.15 req no price) | 737,085 |
| kind notice:team_messages_came | 60 | 15 | 12 | 17.08 | 6,206,280 | 6,151,582 | 54,657 | 40 | 15,160 | $2.31 | 135,778 |

Cost basis (total row): 1,113.72 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

| Runtime | Delivered turns | Tool calls | Mean | Median | p90 | Crew reads | Crew status | Other crew | Non-crew | Turns with a read | First call is a read | Requests in turns | Mean requests |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 4,773 | 28,937 | 6.06 | 1 | 9 | 3,126 | 3,385 | 5,041 | 17,385 | 1,332 | 862 | 21,464 | 4.5 |
| claude | 1,354 | 12,545 | 9.27 | 1 | 16 | 442 | 623 | 1,602 | 9,878 | 248 | 147 | 12,404 | 9.16 |
| codex | 3,419 | 16,392 | 4.79 | 1 | 6 | 2,684 | 2,762 | 3,439 | 7,507 | 1,084 | 715 | 9,060 | 2.65 |

Read tools after deliveries: agents_list 83, tasks_list 88, agent_inbox 2191, task_get 764.

Top agents: codex:content 516, codex:qa 488, codex:native-architect 427, codex:graph 414, claude:reviewer 410, codex:ui 374, codex:runtime 319, codex:designer 284.

## text_only_turn

descriptive: turns with an input that ended with text only (a model response, no tool call), any opener class; associated = every request in the turn.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 848 | 72 | 26 | 848 | 267,123,055 | 240,147,070 | 26,225,579 | 750,406 | 109,653 | $348.01 (+252 req no price) | 0 |
| runtime claude | 595 | 37 | 23 | 596 | 226,082,613 | 199,854,846 | 26,225,579 | 2,188 | 85,567 | $348.01 | 0 |
| runtime codex | 253 | 35 | 11 | 252 | 41,040,442 | 40,292,224 | 0 | 748,218 | 24,086 | no price (+252 req no price) | 0 |
| kind message:answer | 146 | 32 | 17 | 146 | 44,242,589 | 39,106,012 | 4,991,651 | 144,926 | 16,335 | $75.34 (+61 req no price) | 0 |
| kind non_crew | 131 | 10 | 10 | 131 | 55,408,529 | 48,995,564 | 6,412,455 | 510 | 21,600 | $62.19 | 0 |
| kind message:message | 91 | 32 | 16 | 92 | 24,914,124 | 23,136,683 | 1,758,560 | 18,881 | 11,148 | $25.34 (+54 req no price) | 0 |
| kind message:artifact | 83 | 24 | 16 | 83 | 25,075,469 | 24,309,556 | 752,048 | 13,865 | 7,634 | $11.41 (+41 req no price) | 0 |
| kind message:question | 67 | 11 | 8 | 67 | 19,690,241 | 17,813,381 | 1,872,837 | 4,023 | 13,682 | $32.79 (+12 req no price) | 0 |
| kind notice:fast_forward | 62 | 12 | 9 | 62 | 28,462,345 | 25,133,841 | 3,326,101 | 2,403 | 6,230 | $36.54 (+6 req no price) | 0 |
| kind notice:online_idle | 57 | 57 | 23 | 57 | 2,014,462 | 820,162 | 651,371 | 542,929 | 770 | $7.10 (+30 req no price) | 0 |
| kind batch | 57 | 14 | 10 | 57 | 21,453,332 | 20,568,786 | 881,284 | 3,262 | 10,581 | $21.98 (+6 req no price) | 0 |
| kind message:status | 44 | 17 | 11 | 44 | 13,430,879 | 11,744,305 | 1,675,507 | 11,067 | 8,953 | $21.41 (+28 req no price) | 0 |
| kind message:review_result | 33 | 16 | 13 | 32 | 10,542,123 | 8,376,821 | 2,158,536 | 6,766 | 3,094 | $23.74 (+10 req no price) | 0 |
| kind notice:unresponsive | 31 | 2 | 2 | 31 | 8,310,727 | 8,305,148 | 5,513 | 66 | 1,983 | $1.71 | 0 |
| kind notice:task_event | 21 | 3 | 2 | 21 | 4,809,357 | 4,702,897 | 106,400 | 60 | 3,697 | $1.79 | 0 |

Cost basis (total row): 252 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

Top agents: claude:reviewer 124, claude:orchestrator 100, claude:unknown 73, claude:fix-runtime 65, codex:content 60, codex:qa 52, codex:designer 42, claude:designer 28.

## forward_candidate_reply

end-of-turn replies crew may forward (daemon forward_reply): explicit_mention = a reply line opens with @id:, forwarded as a message; message_or_question_from_member_or_orchestrator = opener of type message/question from a non-scheduler sender the turn did not write to, forwarded as an answer only if the opener was addressed directly (not a broadcast; not visible natively); not_forwarded = text-only turns with any other opener (status, scheduler, answer, artifact, notices); associated = the last request of the turn.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 492 | 51 | 23 | 492 | 159,371,643 | 152,021,751 | 7,257,945 | 91,947 | 104,173 | $114.73 (+158 req no price) | 0 |
| runtime claude | 334 | 25 | 20 | 334 | 131,798,692 | 124,539,767 | 7,257,945 | 980 | 91,456 | $114.73 | 0 |
| runtime codex | 158 | 26 | 12 | 158 | 27,572,951 | 27,481,984 | 0 | 90,967 | 12,717 | no price (+158 req no price) | 0 |
| kind message_or_question_from_member_or_orchestrator | 451 | 46 | 20 | 451 | 145,593,945 | 139,818,059 | 5,695,999 | 79,887 | 75,368 | $99.54 (+153 req no price) | 0 |
| kind explicit_mention | 41 | 18 | 14 | 41 | 13,777,698 | 12,203,692 | 1,561,946 | 12,060 | 28,805 | $15.18 (+5 req no price) | 0 |

Cost basis (total row): 158 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

All replies by forwarding class (candidates exclude not_forwarded; 215 candidates were text-only turns, 277 had tool calls):

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| not_forwarded | 533 | 68 | 26 | 533 | 174,795,448 | 155,046,498 | 19,099,278 | 649,672 | 62,956 | $249.14 (+98 req no price) | 0 |
| message_or_question_from_member_or_orchestrator | 451 | 46 | 20 | 451 | 145,593,945 | 139,818,059 | 5,695,999 | 79,887 | 75,368 | $99.54 (+153 req no price) | 0 |
| explicit_mention | 41 | 18 | 14 | 41 | 13,777,698 | 12,203,692 | 1,561,946 | 12,060 | 28,805 | $15.18 (+5 req no price) | 0 |

| Forwarding class | Opener class | Replies |
|---|---|---:|
| message_or_question_from_member_or_orchestrator | message:message | 206 |
| not_forwarded | non_crew | 131 |
| not_forwarded | message:answer | 105 |
| message_or_question_from_member_or_orchestrator | message:question | 95 |
| message_or_question_from_member_or_orchestrator | notice:fast_forward | 85 |
| not_forwarded | message:artifact | 68 |
| message_or_question_from_member_or_orchestrator | batch | 65 |
| not_forwarded | notice:online_idle | 57 |
| not_forwarded | batch | 32 |
| not_forwarded | notice:unresponsive | 31 |
| not_forwarded | message:status | 27 |
| not_forwarded | message:review_result | 26 |
| not_forwarded | notice:task_event | 21 |
| explicit_mention | message:message | 10 |
| not_forwarded | notice:team_messages_came | 8 |
| not_forwarded | notice:fast_forward | 7 |
| explicit_mention | batch | 7 |
| explicit_mention | message:task | 5 |
| explicit_mention | notice:team_messages_came | 5 |
| not_forwarded | message:question | 4 |

Top agents: claude:orchestrator 86, claude:reviewer 67, claude:designer 40, claude:ui 33, codex:designer 28, codex:content 27, codex:graph 25, claude:scheduler 19.

## reminder_turn

turns opened by "[crew] Reminder: task" (idle reminder); candidate = ended with no tool call or only agent_status; Claude sub-kinds say whether a background Bash/Agent/Monitor launch was still un-notified at that moment.

| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 72 | 21 | 12 | 919 | 336,401,414 | 332,741,146 | 2,143,207 | 1,517,061 | 721,472 | $73.40 (+237 req no price) | 0 |
| runtime claude | 36 | 10 | 9 | 682 | 283,709,467 | 281,564,826 | 2,143,207 | 1,434 | 484,676 | $73.40 | 0 |
| runtime codex | 36 | 11 | 7 | 237 | 52,691,947 | 51,176,320 | 0 | 1,515,627 | 236,796 | no price (+237 req no price) | 0 |
| kind background_undetectable_codex | 36 | 11 | 7 | 237 | 52,691,947 | 51,176,320 | 0 | 1,515,627 | 236,796 | no price (+237 req no price) | 0 |
| kind background_running | 19 | 5 | 5 | 310 | 142,889,136 | 141,912,490 | 975,990 | 656 | 277,849 | $36.19 | 0 |
| kind no_background_running | 17 | 7 | 6 | 372 | 140,820,331 | 139,652,336 | 1,167,217 | 778 | 206,827 | $37.21 | 0 |

Cost basis (total row): 237 on models without a listed price (gpt-6-astra, gpt-6.1-sol).

| Runtime | Reminder turns | Ended with no tool call | Only agent_status | Status-only | No model response | Background launch un-notified | …launched within 2h |
|---|---:|---:|---:|---:|---:|---:|---:|
| total | 72 | 3 | 6 | 10 | 14 | 19 | 12 |
| claude | 36 | 2 | 5 | 7 | 0 | 19 | 12 |
| codex | 36 | 1 | 1 | 3 | 14 | not detectable | — |

Candidate reminder turns (no tool call or only agent_status): 23, 15 associated requests, 6,083,606 input tokens, $1.06 (+3 req no price).

Top agents: claude:scheduler 12, codex:qa 10, claude:designer 6, claude:web 6, codex:ui 6, codex:runtime 6, claude:qa 5, codex:content 5.

## agent_inbox calls that returned messages: genuinely new?

| Runtime | Calls with messages | By-id lookups | ≥1 id not delivered in full in-band | ≥1 id not seen anywhere before | All ids already delivered in full | Ids returned | New in-band ids |
|---|---:|---:|---:|---:|---:|---:|---:|
| total | 1660 | 52 | 1426 | 1415 | 231 | 6155 | 4053 |
| claude | 108 | 15 | 62 | 61 | 46 | 1760 | 468 |
| codex | 1552 | 37 | 1364 | 1354 | 185 | 4395 | 3585 |

Types of new ids: answer 1064, status 834, artifact 736, message 674, question 306, review_result 281, task 96, review_request 59, error 3. Senders: orchestrator 587, scheduler 516, content 423, reviewer 412, qa 389, native-architect 334, graph 282, designer 254, native-transport 204, runtime 202.

## Read result sizes (bytes)

| Tool / runtime | Calls | Total | Min | p50 | p90 | Max | Mean | >50 KB | >100 KB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| agent_inbox all | 2,671 | 3,837,279 | 16 | 520 | 3,262 | 46,648 | 1,437 | 0 | 0 |
| agent_inbox claude | 147 | 1,166,354 | 16 | 1,862 | 27,107 | 46,648 | 7,934 | 0 | 0 |
| agent_inbox codex | 2,524 | 2,670,925 | 16 | 498 | 2,861 | 29,921 | 1,058 | 0 | 0 |
| agents_list all | 109 | 186,018 | 68 | 1,627 | 2,517 | 3,018 | 1,707 | 0 | 0 |
| agents_list claude | 59 | 99,669 | 68 | 1,591 | 2,449 | 3,018 | 1,689 | 0 | 0 |
| agents_list codex | 50 | 86,349 | 138 | 1,701 | 2,629 | 2,858 | 1,727 | 0 | 0 |
| git_diff all | 28 | 453,525 | 11 | 12,249 | 49,173 | 49,173 | 16,197 | 0 | 0 |
| git_diff claude | 2 | 83,870 | 34,697 | 49,173 | 49,173 | 49,173 | 41,935 | 0 | 0 |
| git_diff codex | 26 | 369,655 | 11 | 9,591 | 40,547 | 49,173 | 14,218 | 0 | 0 |
| git_status all | 381 | 72,578 | 70 | 123 | 336 | 2,498 | 190 | 0 | 0 |
| git_status claude | 24 | 3,229 | 94 | 130 | 148 | 224 | 135 | 0 | 0 |
| git_status codex | 357 | 69,349 | 70 | 122 | 362 | 2,498 | 194 | 0 | 0 |
| task_get all | 901 | 2,542,531 | 25 | 2,301 | 5,544 | 16,251 | 2,822 | 0 | 0 |
| task_get claude | 303 | 942,480 | 25 | 2,410 | 6,197 | 16,251 | 3,110 | 0 | 0 |
| task_get codex | 598 | 1,600,051 | 27 | 2,267 | 5,282 | 12,907 | 2,676 | 0 | 0 |
| tasks_list all | 137 | 1,777,113 | 11 | 5,238 | 25,171 | 118,535 | 12,972 | 10 | 1 |
| tasks_list claude | 90 | 625,890 | 11 | 5,821 | 15,861 | 45,743 | 6,954 | 0 | 0 |
| tasks_list codex | 47 | 1,151,223 | 11 | 4,395 | 90,522 | 118,535 | 24,494 | 10 | 1 |
| usage_status all | 11 | 1,858 | 122 | 124 | 172 | 475 | 169 | 0 | 0 |
| usage_status claude | 11 | 1,858 | 122 | 124 | 172 | 475 | 169 | 0 | 0 |

Largest tasks_list results: ~/.codex/sessions/2026/10/05/rollout-2026-10-05T12-26-03-01a10d19-e317-76e2-bced-9de15184919c.jsonl:9579 118,535 B; ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-02-01a1199f-2456-71d2-998c-d6c1de01b676.jsonl:899 98,690 B; ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-00-01a1199f-1c2c-7c40-a73f-8fd5256ebdf1.jsonl:31 90,522 B; ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-02-01a1199f-2456-71d2-998c-d6c1de01b676.jsonl:20 90,522 B; ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-02-01a1199f-2456-71d2-998c-d6c1de01b676.jsonl:37 90,522 B.

## Turns by input class (text-only = a model response without any tool call)

| Runtime + input class | Turns | Text-only | Status-only | No model response | Text-only associated requests | Text-only input tokens | Text-only associated list cost |
|---|---:|---:|---:|---:|---:|---:|---:|
| codex message:answer | 913 | 61 | 237 | 356 | 61 | 10,763,214 | no price (+61 req no price) |
| codex message:artifact | 651 | 41 | 141 | 289 | 41 | 8,752,393 | no price (+41 req no price) |
| claude non_crew | 598 | 131 | 75 | 10 | 131 | 55,408,529 | $62.19 |
| codex message:message | 479 | 54 | 127 | 138 | 54 | 8,348,723 | no price (+54 req no price) |
| codex message:status | 423 | 28 | 110 | 186 | 28 | 5,019,141 | no price (+28 req no price) |
| codex message:question | 249 | 12 | 20 | 102 | 12 | 2,676,957 | no price (+12 req no price) |
| codex message:review_result | 207 | 11 | 51 | 72 | 10 | 1,856,794 | no price (+10 req no price) |
| claude batch | 193 | 51 | 42 | 1 | 51 | 21,036,450 | $21.98 |
| codex notice:no_tool_call | 161 | 0 | 60 | 55 | 0 | — | — |
| codex notice:usage_lifted | 142 | 0 | 0 | 113 | 0 | — | — |
| claude message:answer | 141 | 85 | 82 | 0 | 85 | 33,479,375 | $75.34 |
| claude message:message | 134 | 37 | 37 | 7 | 38 | 16,565,401 | $25.34 |
| claude message:artifact | 121 | 42 | 39 | 4 | 42 | 16,323,076 | $11.41 |
| claude notice:fast_forward | 120 | 56 | 58 | 0 | 56 | 26,446,462 | $36.54 |
| claude message:question | 113 | 55 | 31 | 0 | 55 | 17,013,284 | $32.79 |
| claude message:review_result | 93 | 22 | 30 | 0 | 22 | 8,685,329 | $23.74 |
| codex batch | 93 | 6 | 15 | 17 | 6 | 416,882 | no price (+6 req no price) |
| claude message:review_request | 83 | 2 | 3 | 0 | 2 | 902,852 | $0.21 |
| codex message:task | 77 | 0 | 0 | 19 | 0 | — | — |
| codex message:review_request | 65 | 0 | 3 | 0 | 0 | — | — |
| claude notice:team_messages_came | 60 | 9 | 13 | 0 | 9 | 4,477,646 | $14.47 |
| claude message:status | 57 | 16 | 21 | 1 | 16 | 8,411,738 | $21.41 |
| claude notice:unresponsive | 57 | 31 | 30 | 0 | 31 | 8,310,727 | $1.71 |
| claude message:task | 55 | 0 | 0 | 0 | 0 | — | — |
| claude notice:task_event | 51 | 21 | 8 | 0 | 21 | 4,809,357 | $1.79 |
| codex notice:fast_forward | 39 | 6 | 14 | 12 | 6 | 2,015,883 | no price (+6 req no price) |
| claude notice:task_reminder | 36 | 2 | 7 | 0 | 2 | 780,167 | $0.16 |
| codex notice:task_reminder | 36 | 1 | 3 | 14 | 1 | 79,348 | no price (+1 req no price) |
| claude batch:status | 35 | 2 | 1 | 0 | 2 | 428,408 | $0.13 |
| codex notice:online_idle | 30 | 30 | 30 | 0 | 30 | 751,643 | no price (+30 req no price) |

Failed turns (native turn error, usually no model response): codex Your workspace is out of credits. Ask your workspace owner to refill i ×806; codex Your workspace is out of credits. Add credits to continue. ×472; codex exceeded retry limit, last status: N Too Many Requests, request id: <s ×148; codex Error running remote compact task: Your workspace is out of credits. A ×26; codex Selected model is at capacity. Please try a different model. ×5; codex This content was flagged for possible cybersecurity risk. If this seem ×3.

## Crew input openings (top 20, all [crew] inputs)

| Opening | Class | Count | Turn start | Folded | Claude | Codex |
|---|---|---:|---:|---:|---:|---:|
| [crew] Message from <agent> (type answer) | message:answer | 1301 | 1054 | 247 | 387 | 914 |
| [crew] Message from <agent> (type artifact) | message:artifact | 973 | 772 | 201 | 322 | 651 |
| [crew] Message from <agent> (type message) | message:message | 883 | 613 | 270 | 404 | 479 |
| [crew] Message from <agent> (type status) | message:status | 637 | 480 | 157 | 214 | 423 |
| [crew] Message from <agent> (type review_result) | message:review_result | 443 | 300 | 143 | 236 | 207 |
| [crew] Message from <agent> (type question) | message:question | 441 | 362 | 79 | 192 | 249 |
| [crew] N new messages: | batch | 330 | 326 | 4 | 231 | 99 |
| [crew] Message from <agent> (type review_request) | message:review_request | 283 | 148 | 135 | 218 | 65 |
| [crew] Message from <agent> (type status): [crew] Your last turn ended without a tool call | notice:no_tool_call | 199 | 162 | 37 | 37 | 162 |
| [crew] Message from <agent> (type message): main fast-forwarded to <sha> | notice:fast_forward | 180 | 101 | 79 | 155 | 25 |
| [crew] Message from <agent> (type task) | message:task | 170 | 132 | 38 | 91 | 79 |
| [crew] The usage limit that stopped you has lifted. Continue your current task where you l | notice:usage_lifted | 142 | 142 | 0 | 0 | 142 |
| [crew] Reminder: task "…" (id …) is still open. Continue working on it; call task_complete | notice:task_reminder | 72 | 72 | 0 | 36 | 36 |
| [crew] Message from <agent> (type status): Task <id> (…) was completed by <agent> | notice:task_event | 66 | 62 | 4 | 66 | 0 |
| [crew] Team messages came as your last turn ended (above). Act on them. | notice:team_messages_came | 60 | 60 | 0 | 60 | 0 |
| [crew] Message from <agent> (type status): Agent <agent> is unresponsive | notice:unresponsive | 58 | 57 | 1 | 58 | 0 |
| [crew] You are online as <agent>. Nothing is assigned to you yet. Reply with one short lin | notice:online_idle | 57 | 57 | 0 | 27 | 30 |
| [crew] Message from <agent> (type status): Your conversation was just compacted | notice:compacted | 44 | 43 | 1 | 16 | 28 |
| [crew] Message from <agent> (type status): Card <id> revised by orchestrator | notice:card_revised | 36 | 28 | 8 | 8 | 28 |
| [crew] Message from <agent> (type status): <agent>'s model | notice:refusal | 17 | 14 | 3 | 17 | 0 |

## Notice and status openings (top 20)

| Opening | Class | Count | Turn start | Folded | Top senders |
|---|---|---:|---:|---:|---|
| [crew] Message from <agent> (type status) | message:status | 637 | 480 | 157 | qa 126, orchestrator 92, scheduler 75 |
| [crew] Message from <agent> (type status): [crew] Your last turn ended without a tool call | notice:no_tool_call | 199 | 162 | 37 | scheduler 199 |
| [crew] Message from <agent> (type message): main fast-forwarded to <sha> | notice:fast_forward | 180 | 101 | 79 | orchestrator 180 |
| [crew] The usage limit that stopped you has lifted. Continue your current task where you l | notice:usage_lifted | 142 | 142 | 0 |  |
| [crew] Reminder: task "…" (id …) is still open. Continue working on it; call task_complete | notice:task_reminder | 72 | 72 | 0 |  |
| [crew] Message from <agent> (type status): Task <id> (…) was completed by <agent> | notice:task_event | 66 | 62 | 4 | scheduler 66 |
| [crew] Team messages came as your last turn ended (above). Act on them. | notice:team_messages_came | 60 | 60 | 0 |  |
| [crew] Message from <agent> (type status): Agent <agent> is unresponsive | notice:unresponsive | 58 | 57 | 1 | scheduler 58 |
| [crew] You are online as <agent>. Nothing is assigned to you yet. Reply with one short lin | notice:online_idle | 57 | 57 | 0 |  |
| [crew] Message from <agent> (type status): Your conversation was just compacted | notice:compacted | 44 | 43 | 1 | scheduler 44 |
| [crew] Message from <agent> (type status): Card <id> revised by orchestrator | notice:card_revised | 36 | 28 | 8 | scheduler 36 |
| [crew] Message from <agent> (type status): <agent>'s model | notice:refusal | 17 | 14 | 3 | scheduler 17 |
| [crew] Message from <agent> (type status): <agent> cannot act on your message m-X | notice:refusal | 12 | 3 | 9 | scheduler 12 |
| [crew] Message from <agent> (type message): Lead change: orchestrator now Claude Code/Opus | notice:fast_forward | 8 | 6 | 2 | orchestrator 8 |
| [crew] Message from <agent> (type message): Correction: reviewer confirmed approval covers graph chain to <sha> | notice:fast_forward | 8 | 3 | 5 | orchestrator 8 |
| [crew] Message from <agent> (type status): Agent qa (Codex) hit a usage limit | notice:usage_limit | 3 | 2 | 1 | scheduler 3 |
| [crew] Message from <agent> (type message): native-linux-graphs approved | notice:fast_forward | 3 | 0 | 3 | graph 2, orchestrator 1 |
| [crew] Message from <agent> (type artifact): Final fixture source <sha> after <sha> | notice:fast_forward | 3 | 1 | 2 | content 3 |
| [crew] Fable week has reset: your session was restarted on Fable N, your own model again.  | notice:model_restored | 2 | 2 | 0 |  |
| [crew] Message from <agent> (type status): All N tasks are done | notice:all_done | 2 | 1 | 1 | scheduler 2 |

## Limits

- These are candidates, not findings of avoidable cost or savings. Cohorts are selected by tool names, result strings and message wrappers only; a candidate call or turn may be useful work (analysis written as text, reviews, waiting/parking transitions, continuations, a status the team needed).
- Association is an upper bound, not avoidable cost. A candidate call (inbox_empty, inbox_duplicate_only, inbox_after_delivery, repeated_read) is "alone" when every tool call consumed by the same next model response is agent_inbox/agent_status/task_update; it is then associated with that whole consuming request (deduplicated per pattern), which may also have done other reasoning. A call bundled with other real tool work (Codex exec with exec_command etc., Claude parallel tool_use) is associated with its result bytes only. The issuing response is never associated. delivery_round_trips splits consuming requests among sibling calls. Turn patterns associate every request in the turn; forward_candidate_reply associates the turn's last request.
- A call joins its exact native consuming request: the request recorded after the next model response and before the model response after that (by source line); otherwise it has none. Joins use all requests of the session; --since/--until (inclusive, like baseline) then select subjects (the call, or the turn's start/input) and associated requests. A subject with an unknown timestamp is outside a set window. State before the window still feeds delivered ids and post-delivery windows.
- List cost is input side only (uncached tail × base, cache writes × 5m/1h write price, cache reads × read price) from notes/prompt-cache-audit/prices.tsv; output tokens are not priced. A write without a recorded 5m/1h split is priced as a range from the cheaper to the dearer write price; a missing input-side field leaves only a lower bound ("at least"); a request with no usage fields is "usage unknown"; models without a listed price (Codex gpt-*) are "no price". Cache writes are reported as writes; nothing here infers a cold cache.
- Delivered ids are only those whose full body reached the member in-band: the header id of a non-empty "[crew] Message from" input and each block of a "[crew] N new messages" batch, unless the block was clipped ("… (truncated"). Ids merely mentioned in notices or bodies are references and never count as delivered.
- forward_candidate_reply follows crew's forward_reply rules as far as native files show them: the opener's recipient (direct vs broadcast "*") is not recorded natively, so message_or_question_from_member_or_orchestrator is an upper bound; explicit_mention matches a reply line opening with @name(s): without checking the names against the roster.
- Background work at reminder time is Claude-only: Bash run_in_background, async Agent or Monitor launches confirmed by their tool result and not yet named by a <task-notification>. A process restart can orphan a launch, so the un-notified count is an upper bound; the 2h column limits it to recent launches. Codex background terminals are not detectable from the rollout.
- Inbox novelty compares returned message ids with ids delivered in full in-band earlier in the same session file; a session that began after an earlier delivery (restart, takeover) can make an old id look new.
- Claude requests are deduplicated by message id with the last streamed usage winning (baseline.mjs). Codex requests are token_count cumulative deltas per sampling; a Codex inner tool call (crew MCP call inside an `exec` code-mode wrapper) returns with its wrapper output, so all inner calls of one wrapper share the consuming request. When two exec wrappers run in one sampling, inner calls are linked to the first wrapper output after them.
- Patterns overlap (an empty inbox call after a delivery appears in inbox_empty and inbox_after_delivery; a notice turn may also be a status-only turn). Do not add pattern totals.
- Status-only is inferred from tool names and agent_send type only. Short reply = total assistant text in the turn ≤ the threshold, with no tool calls.
- Repeated-read detection resets on any own crew mutating call (task_*, review_*, git_commit/merge/cherry_pick, agent_status, agent lifecycle) and, for git_status/git_diff, on any own file-modifying tool call. Other agents may have changed state invisibly; identical bytes mean the read returned nothing new.
- Mid-turn folded crew messages (Claude isMeta, Codex inputs after model output in the same task) count as deliveries for the inbox window and openings, but are not turns and carry no turn requests.
- With --until set, each measured turn is seen only up to the cutoff (its later or undated events and requests are dropped); a turn with no end marker (Claude turn_duration, Codex task_complete) and no next turn by the cutoff is counted as partial. Appending records after the cutoff does not change the result.
- Token columns show the sum over associated requests that report the metric; "(K of N)" means only K of N associated requests report it and "not reported" that none does (e.g. Codex has no native 5m/1h write split, and a missing native write counter stays unknown, never 0). Cost coverage is reported separately and does not qualify token columns.
- Sessions are read up to the byte size baseline.mjs read; later appends are excluded.
