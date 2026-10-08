# Turn efficiency

Generated 2026-10-08T07:13:51.813Z from native session files (cohort and usage via baseline.mjs). Window N=3 model responses; short reply ≤ 300 chars.

## Cohort

| Runtime | Sessions | Requests | Turns | Tool calls | Crew tool calls | Codex exec wrappers | Agents |
|---|---:|---:|---:|---:|---:|---:|---|
| claude | 49 | 17,604 | 1,876 | 18,034 | 3,547 | 0 | architect, auditor, baseline, content, designer, fix-core, fix-runtime, graph, integrator, lead, linux, native-architect, orchestrator, qa, rev-tui, reviewer, sandbox, scheduler, tui, ui, unknown, vcs, vcs-batchtests, vcs-fallback, vcs-hubtests, vcs-mismatch, vcs-pin, vcs-pinfix, vcs-pinfix2, vcs-wakeups, web |
| codex | 45 | 9,621 | 3,486 | 18,327 | 9,720 | 7,602 | architect, cache-reviewer, content, designer, graph, native-architect, native-transport, qa, reviewer, runtime, sandbox, scheduler, ui, vcs-pinfix2, web |

agent_inbox result kinds: claude messages=104, id_not_found=27, messages_persisted=1, empty=1, error=9; codex messages=1474, id_not_found=21, empty=867, error=5.

Non-message inbox result strings (normalized): `No new messages.` ×868, `no message 'm-X' sent or received by you` ×47, `incompatible crew protocol: client speaks protocol N, daemon speaks N; restart t` ×13, `<persisted-output>` ×1, `Error: result (N,N characters across N lines) exceeds maximum allowed toke` ×1, `no message 'w-X' sent or received by you` ×1.

## inbox_empty

agent_inbox calls whose result says no new messages.

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 868 | 29 | 14 | 315 | 90,089,283 | 89,913,216 | 0 | 176,067 | 38,255 | no price (+315 req no price) | 13,888 |
| runtime codex | 867 | 28 | 13 | 315 | 90,089,283 | 89,913,216 | 0 | 176,067 | 38,255 | no price (+315 req no price) | 13,872 |
| runtime claude | 1 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 16 |
| kind bundled | 550 | 29 | 14 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 8,800 |
| kind alone | 315 | 19 | 11 | 315 | 90,089,283 | 89,913,216 | 0 | 176,067 | 38,255 | no price (+315 req no price) | 5,040 |
| kind no_consumer | 3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 48 |

Top agents: codex:content 243, codex:native-architect 108, codex:graph 106, codex:scheduler 89, codex:qa 87, codex:runtime 62, codex:ui 45, codex:native-transport 44.

## inbox_after_delivery

agent_inbox calls within N model responses after an in-band delivery that already carried the message bodies (wasted = empty or duplicate-only result).

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 623 | 38 | 16 | 358 | 99,539,428 | 99,294,192 | 72,026 | 173,210 | 12,274 | $1.76 (+348 req no price) | 583,703 |
| runtime codex | 596 | 25 | 11 | 348 | 96,226,692 | 96,053,504 | 0 | 173,188 | 8,085 | no price (+348 req no price) | 205,005 |
| runtime claude | 27 | 13 | 11 | 10 | 3,312,736 | 3,240,688 | 72,026 | 22 | 4,189 | $1.76 | 378,698 |
| kind empty alone | 290 | 13 | 9 | 290 | 82,911,391 | 82,774,144 | 0 | 137,247 | 4,123 | no price (+290 req no price) | 4,640 |
| kind empty bundled | 147 | 19 | 10 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 2,352 |
| kind duplicate_only bundled | 115 | 30 | 14 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 343,379 |
| kind duplicate_only alone | 69 | 16 | 13 | 68 | 16,628,037 | 16,520,048 | 72,026 | 35,963 | 8,151 | $1.76 (+58 req no price) | 233,300 |
| kind empty no_consumer | 2 | 2 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 32 |

All inbox calls inside a post-delivery window: 1049 (new_messages alone 144, duplicate_only alone 69, id_not_found bundled 26, duplicate_only bundled 115, new_messages bundled 242, id_not_found alone 8, error alone 3, error bundled 3, empty alone 290, empty bundled 147, empty no_consumer 2).

Top agents: codex:content 204, codex:native-architect 92, codex:qa 72, codex:graph 56, codex:designer 37, codex:native-transport 30, codex:ui 26, codex:cache-reviewer 26.

## inbox_duplicate_only

agent_inbox calls whose returned message ids were all already delivered in-band earlier in the session.

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 226 | 40 | 20 | 78 | 18,815,482 | 18,681,239 | 96,504 | 37,739 | 10,887 | $2.37 (+59 req no price) | 780,759 |
| runtime codex | 182 | 22 | 11 | 59 | 13,430,211 | 13,392,512 | 0 | 37,699 | 4,159 | no price (+59 req no price) | 270,606 |
| runtime claude | 44 | 18 | 16 | 19 | 5,385,271 | 5,288,727 | 96,504 | 40 | 6,728 | $2.37 | 510,153 |
| kind bundled | 147 | 37 | 18 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 486,314 |
| kind alone | 79 | 22 | 17 | 78 | 18,815,482 | 18,681,239 | 96,504 | 37,739 | 10,887 | $2.37 (+59 req no price) | 294,445 |

Top agents: codex:qa 54, codex:content 29, codex:designer 26, codex:reviewer 19, codex:graph 18, codex:cache-reviewer 11, codex:runtime 8, codex:native-architect 8.

## repeated_read

read tool calls (tasks_list, task_get, agents_list, git_status, git_diff, usage_status, agent_inbox by id) byte-identical to the previous same tool+args call with no own state change between.

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 7 | 6 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 188,740 |
| runtime codex | 6 | 5 | 4 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 184,147 |
| runtime claude | 1 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 4,593 |
| kind task_get bundled | 4 | 4 | 4 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 9,377 |
| kind tasks_list bundled | 2 | 2 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 179,243 |
| kind git_status bundled | 1 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | no price | 120 |

Top agents: codex:graph 3, claude:unknown 1, codex:runtime 1, codex:native-architect 1, codex:qa 1.

## status_only_turn

turns whose only tool calls are agent_status / task_update / agent_send type=status, or with no tool calls and a short reply.

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 1,178 | 73 | 26 | 1,736 | 484,281,345 | 460,558,012 | 20,773,351 | 2,949,982 | 137,047 | $284.04 (+1,184 req no price) | 0 |
| runtime codex | 710 | 36 | 11 | 1,184 | 274,318,484 | 271,370,368 | 0 | 2,948,116 | 80,570 | no price (+1,184 req no price) | 0 |
| runtime claude | 468 | 37 | 23 | 552 | 209,962,861 | 189,187,644 | 20,773,351 | 1,866 | 56,477 | $284.04 | 0 |
| kind message | 799 | 46 | 20 | 1,230 | 329,848,051 | 317,864,024 | 9,995,144 | 1,988,883 | 94,324 | $162.61 (+990 req no price) | 0 |
| kind notice | 250 | 70 | 26 | 341 | 93,365,972 | 86,394,550 | 6,141,501 | 829,921 | 22,504 | $72.26 (+170 req no price) | 0 |
| kind non_crew | 75 | 10 | 10 | 98 | 40,465,993 | 36,705,574 | 3,760,089 | 330 | 12,369 | $28.27 | 0 |
| kind batch | 54 | 20 | 14 | 67 | 20,601,329 | 19,593,864 | 876,617 | 130,848 | 7,850 | $20.90 (+24 req no price) | 0 |

Top agents: codex:designer 149, codex:qa 145, claude:reviewer 121, codex:graph 75, codex:content 71, codex:ui 69, codex:runtime 57, claude:unknown 54.

## status_or_read_only_turn

turns (not already status-only) whose tool calls are only status calls plus crew reads (agent_inbox, tasks_list, task_get, agents_list).

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 487 | 37 | 19 | 1,011 | 264,773,205 | 262,178,467 | 603,299 | 1,991,439 | 84,198 | $10.54 (+928 req no price) | 0 |
| runtime codex | 452 | 26 | 11 | 928 | 241,179,695 | 239,188,480 | 0 | 1,991,215 | 69,935 | no price (+928 req no price) | 0 |
| runtime claude | 35 | 11 | 9 | 83 | 23,593,510 | 22,989,987 | 603,299 | 224 | 14,263 | $10.54 | 0 |
| kind message | 408 | 28 | 14 | 839 | 220,222,387 | 218,950,304 | 24,637 | 1,247,446 | 62,397 | $1.45 (+822 req no price) | 0 |
| kind notice | 59 | 23 | 13 | 130 | 31,994,194 | 31,498,799 | 164,036 | 331,359 | 15,010 | $4.42 (+78 req no price) | 0 |
| kind batch | 15 | 6 | 6 | 30 | 7,803,337 | 7,389,522 | 1,215 | 412,600 | 4,678 | $0.13 (+28 req no price) | 0 |
| kind non_crew | 5 | 3 | 3 | 12 | 4,753,287 | 4,339,842 | 413,411 | 34 | 2,113 | $4.54 | 0 |

Top agents: codex:content 172, codex:native-architect 70, codex:qa 43, codex:graph 42, codex:designer 37, codex:ui 36, claude:unknown 19, codex:runtime 19.

## notice_turn

turns started by crew notices or status messages (no-action candidates); wasted = the turn was status-only or status/read-only.

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 486 | 73 | 26 | 789 | 212,011,461 | 201,892,560 | 7,913,629 | 2,205,272 | 66,555 | $103.62 (+546 req no price) | 0 |
| runtime codex | 302 | 36 | 11 | 546 | 130,446,488 | 128,241,920 | 0 | 2,204,568 | 40,245 | no price (+546 req no price) | 0 |
| runtime claude | 184 | 37 | 23 | 243 | 81,564,973 | 73,650,640 | 7,913,629 | 704 | 26,310 | $103.62 | 0 |
| kind message:status | 173 | 30 | 15 | 311 | 85,579,396 | 82,932,350 | 1,606,456 | 1,040,590 | 27,673 | $26.77 (+294 req no price) | 0 |
| kind notice:no_tool_call | 81 | 24 | 11 | 164 | 40,032,978 | 39,681,792 | 0 | 351,186 | 9,985 | no price (+164 req no price) | 0 |
| kind notice:fast_forward | 73 | 16 | 12 | 94 | 39,813,234 | 35,372,136 | 4,192,446 | 248,652 | 8,563 | $44.78 (+22 req no price) | 0 |
| kind notice:online_idle | 57 | 57 | 23 | 57 | 2,014,462 | 820,162 | 651,371 | 542,929 | 770 | $7.10 (+30 req no price) | 0 |
| kind notice:unresponsive | 37 | 2 | 2 | 45 | 11,695,795 | 11,678,095 | 17,606 | 94 | 3,416 | $2.48 | 0 |
| kind notice:team_messages_came | 17 | 9 | 8 | 33 | 13,389,442 | 12,065,012 | 1,324,326 | 104 | 4,380 | $18.87 | 0 |
| kind notice:task_event | 12 | 2 | 1 | 17 | 3,130,735 | 3,030,576 | 100,113 | 46 | 2,936 | $1.41 | 0 |
| kind notice:task_reminder | 9 | 6 | 6 | 19 | 7,464,992 | 7,453,423 | 10,761 | 808 | 2,695 | $1.41 (+2 req no price) | 0 |
| kind notice:card_revised | 8 | 7 | 6 | 16 | 4,011,915 | 4,004,096 | 0 | 7,819 | 1,143 | no price (+16 req no price) | 0 |
| kind notice:compacted | 7 | 6 | 6 | 14 | 1,003,778 | 994,176 | 0 | 9,602 | 763 | no price (+14 req no price) | 0 |
| kind notice:refusal | 6 | 3 | 3 | 10 | 2,372,599 | 2,364,012 | 8,555 | 32 | 2,651 | $0.54 | 0 |
| kind batch:status | 4 | 2 | 2 | 7 | 1,071,899 | 1,066,861 | 1,636 | 3,402 | 1,368 | $0.17 (+4 req no price) | 0 |

All notice/status-started turns: 1296.

| Input class | Turns | Wasted (status/read-only) | Status-only | Requests | Input total | Cache read | List cost |
|---|---:|---:|---:|---:|---:|---:|---:|
| message:status | 437 | 173 | 106 | 1,016 | 271,706,640 | 262,808,759 | $53.48 |
| notice:no_tool_call | 154 | 81 | 53 | 223 | 55,753,780 | 55,349,436 | $1.70 |
| notice:fast_forward | 153 | 73 | 71 | 445 | 186,550,920 | 180,870,655 | $86.01 |
| notice:usage_lifted | 142 | 0 | 0 | 916 | 271,325,242 | 264,932,736 | no price |
| notice:task_reminder | 61 | 9 | 7 | 865 | 312,925,718 | 309,336,052 | $68.45 |
| notice:online_idle | 57 | 57 | 57 | 57 | 2,014,462 | 820,162 | $7.10 |
| notice:unresponsive | 57 | 37 | 30 | 106 | 27,946,947 | 27,886,392 | $6.06 |
| notice:team_messages_came | 55 | 17 | 13 | 338 | 129,551,400 | 124,561,463 | $91.65 |
| notice:task_event | 49 | 12 | 7 | 101 | 22,666,890 | 22,381,688 | $6.76 |
| batch:status | 37 | 4 | 1 | 136 | 25,128,583 | 24,851,323 | $6.54 |
| notice:compacted | 37 | 7 | 5 | 994 | 175,935,766 | 173,052,482 | $61.64 |
| notice:card_revised | 28 | 8 | 2 | 52 | 11,735,728 | 11,633,536 | no price |
| notice:refusal | 17 | 6 | 3 | 44 | 7,061,299 | 7,020,176 | $1.73 |
| notice:usage_limit | 6 | 2 | 2 | 16 | 3,222,058 | 3,201,963 | $0.80 |
| notice:restarted | 3 | 0 | 0 | 30 | 14,453,531 | 13,115,392 | no price |
| notice:model_restored | 2 | 0 | 0 | 10 | 4,319,536 | 3,445,167 | $18.35 |
| notice:all_done | 1 | 0 | 0 | 4 | 984,509 | 981,547 | $0.22 |

Turn-starting status messages by sender (scheduler = daemon notices; a teammate status starts a turn only when the recipient was waiting):

| Sender | Turns | Wasted (status/read-only) | Status-only | Requests (all) | Input total (all) | List cost (all) | Requests (wasted) | List cost (wasted) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| qa | 107 | 47 | 25 | 167 | 41,119,846 | $0.25 | 86 | no price |
| orchestrator | 71 | 19 | 10 | 92 | 30,175,233 | no price | 34 | no price |
| scheduler | 41 | 14 | 9 | 92 | 26,362,120 | $23.09 | 25 | $11.31 |
| graph | 40 | 18 | 11 | 135 | 23,288,608 | $2.55 | 36 | $0.07 |
| content | 32 | 10 | 8 | 46 | 16,883,133 | $0.85 | 18 | no price |
| designer | 27 | 9 | 8 | 47 | 13,935,819 | $0.05 | 15 | $0.05 |
| runtime | 24 | 14 | 8 | 146 | 34,930,944 | $9.62 | 27 | no price |
| native-transport | 24 | 9 | 2 | 29 | 9,327,445 | no price | 18 | no price |
| ui | 22 | 15 | 12 | 197 | 52,130,483 | $0.34 | 24 | $0.34 |
| native-architect | 18 | 1 | 0 | 6 | 2,048,472 | no price | 2 | no price |
| reviewer | 15 | 6 | 3 | 32 | 8,461,976 | $0.34 | 9 | $0.16 |
| vcs-pinfix2 | 4 | 3 | 3 | 10 | 6,142,967 | $1.11 | 5 | $0.18 |
| web | 3 | 3 | 3 | 5 | 1,805,653 | $10.06 | 5 | $10.06 |
| integrator | 3 | 1 | 1 | 5 | 1,458,218 | $0.25 | 2 | no price |
| cache-reviewer | 3 | 2 | 1 | 3 | 601,415 | $1.26 | 3 | $1.26 |
| sandbox | 1 | 1 | 1 | 1 | 422,424 | $3.16 | 1 | $3.16 |
| vcs-pin | 1 | 1 | 1 | 1 | 870,208 | $0.18 | 1 | $0.18 |
| vcs-fallback | 1 | 0 | 0 | 2 | 1,741,676 | $0.35 | 0 | no price |

Top agents: codex:content 59, claude:unknown 53, codex:qa 50, codex:designer 49, claude:reviewer 27, codex:runtime 27, codex:native-architect 25, codex:graph 23.

## delivery_round_trips

turns started by a delivered crew message: tool calls by category; tokens = requests that consumed the crew read calls.

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 4,424 | 60 | 29 | 1,320.29 | 330,937,179 | 326,009,098 | 706,305 | 4,221,776 | 1,094,272 | $27.45 (+1,022.76 req no price) | 5,148,721 |
| runtime codex | 3,217 | 28 | 11 | 1,022.76 | 247,811,452 | 243,590,288 | 0 | 4,221,164 | 933,892 | no price (+1,022.76 req no price) | 3,557,964 |
| runtime claude | 1,207 | 32 | 26 | 297.52 | 83,125,728 | 82,418,810 | 706,305 | 612 | 160,380 | $27.45 | 1,590,757 |
| kind message:answer | 954 | 41 | 18 | 236.84 | 69,801,668 | 69,169,641 | 43,193 | 588,834 | 224,925 | $1.16 (+227.84 req no price) | 699,391 |
| kind message:artifact | 739 | 41 | 19 | 127.36 | 31,022,704 | 30,534,106 | 18,190 | 470,408 | 82,855 | $0.71 (+118.36 req no price) | 316,075 |
| kind message:message | 555 | 47 | 20 | 156.27 | 35,483,907 | 34,007,917 | 37,675 | 1,438,315 | 139,145 | $1.25 (+140.77 req no price) | 663,764 |
| kind message:status | 437 | 37 | 16 | 125.48 | 33,766,817 | 33,469,598 | 23,068 | 274,152 | 137,908 | $0.67 (+116.98 req no price) | 351,805 |
| kind message:question | 333 | 33 | 14 | 87.5 | 24,786,665 | 24,509,699 | 13,609 | 263,357 | 120,454 | $0.92 (+80 req no price) | 315,181 |
| kind message:review_result | 284 | 45 | 19 | 49.26 | 13,168,258 | 12,888,570 | 10,915 | 268,773 | 32,729 | $0.44 (+41.76 req no price) | 117,786 |
| kind batch | 253 | 39 | 18 | 116.39 | 26,241,933 | 25,900,275 | 52,861 | 288,798 | 110,739 | $3.12 (+88.06 req no price) | 558,483 |
| kind notice:no_tool_call | 154 | 27 | 11 | 21.92 | 5,263,675 | 5,217,685 | 0 | 45,990 | 5,155 | no price (+21.92 req no price) | 86,157 |
| kind notice:fast_forward | 153 | 21 | 14 | 11.67 | 3,323,746 | 3,284,707 | 32,266 | 6,774 | 6,589 | $0.89 (+2.67 req no price) | 90,337 |
| kind message:review_request | 145 | 8 | 4 | 86.27 | 23,794,919 | 23,575,175 | 85,839 | 133,905 | 48,602 | $4.99 (+47.42 req no price) | 437,057 |
| kind message:task | 128 | 49 | 24 | 192.17 | 43,070,602 | 42,573,162 | 121,998 | 375,442 | 130,591 | $6.16 (+110.34 req no price) | 707,499 |
| kind notice:unresponsive | 57 | 2 | 2 | 18 | 4,565,295 | 4,540,640 | 24,619 | 36 | 5,644 | $1.11 | 61,456 |

| Runtime | Delivered turns | Tool calls | Mean | Median | p90 | Crew reads | Crew status | Other crew | Non-crew | Turns with a read | First call is a read | Requests in turns | Mean requests |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 4,424 | 26,761 | 6.05 | 1 | 9 | 2,892 | 3,057 | 4,700 | 16,112 | 1,243 | 799 | 19,700 | 4.45 |
| claude | 1,207 | 11,619 | 9.63 | 1 | 17 | 406 | 523 | 1,469 | 9,221 | 228 | 143 | 11,464 | 9.5 |
| codex | 3,217 | 15,142 | 4.71 | 1 | 6 | 2,486 | 2,534 | 3,231 | 6,891 | 1,015 | 656 | 8,236 | 2.56 |

Read tools after deliveries: agents_list 82, tasks_list 85, agent_inbox 2029, task_get 696.

Top agents: codex:qa 455, codex:content 453, codex:native-architect 403, claude:reviewer 392, codex:graph 392, codex:ui 374, codex:designer 284, codex:runtime 259.

## text_only_reply_turn

turns opened by a delivered orchestrator message (type message) or any status-type message that ended with text only (model replied, no tool call); crew forwards such replies as answers.

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 186 | 32 | 18 | 186 | 60,556,238 | 56,003,756 | 4,532,116 | 20,366 | 22,302 | $57.55 (+54 req no price) | 0 |
| runtime claude | 132 | 17 | 14 | 132 | 50,773,354 | 46,240,812 | 4,532,116 | 426 | 14,442 | $57.55 | 0 |
| runtime codex | 54 | 15 | 9 | 54 | 9,782,884 | 9,762,944 | 0 | 19,940 | 7,860 | no price (+54 req no price) | 0 |
| kind from orchestrator type message (notice:fast_forward) | 52 | 9 | 7 | 52 | 23,725,147 | 21,650,691 | 2,072,882 | 1,574 | 4,385 | $25.85 (+5 req no price) | 0 |
| kind from orchestrator type message (message:message) | 33 | 19 | 13 | 33 | 9,083,487 | 9,045,099 | 30,973 | 7,415 | 2,373 | $1.57 (+21 req no price) | 0 |
| kind from scheduler type status (notice:unresponsive) | 31 | 2 | 2 | 31 | 8,310,727 | 8,305,148 | 5,513 | 66 | 1,983 | $1.71 | 0 |
| kind from member type status (message:status) | 31 | 11 | 8 | 31 | 9,274,892 | 8,711,886 | 553,625 | 9,381 | 7,397 | $5.27 (+22 req no price) | 0 |
| kind from scheduler type status (notice:task_event) | 20 | 3 | 2 | 20 | 4,381,720 | 4,275,662 | 106,002 | 56 | 3,618 | $1.70 | 0 |
| kind from scheduler type status (message:status) | 5 | 3 | 3 | 5 | 2,629,240 | 1,507,937 | 1,121,283 | 20 | 947 | $15.97 | 0 |
| kind from orchestrator type status (message:status) | 4 | 4 | 4 | 4 | 524,231 | 523,136 | 0 | 1,095 | 455 | no price (+4 req no price) | 0 |
| kind from scheduler type status (notice:fast_forward) | 3 | 2 | 2 | 3 | 1,209,952 | 569,237 | 640,707 | 8 | 361 | $5.24 | 0 |
| kind from scheduler type status (notice:usage_limit) | 3 | 2 | 2 | 3 | 745,351 | 744,806 | 533 | 12 | 323 | $0.15 | 0 |
| kind from scheduler type status (notice:refusal) | 2 | 2 | 2 | 2 | 393,896 | 393,290 | 598 | 8 | 347 | $0.08 | 0 |
| kind from scheduler type status (notice:card_revised) | 1 | 1 | 1 | 1 | 213,754 | 213,376 | 0 | 378 | 93 | no price (+1 req no price) | 0 |
| kind from scheduler type status (notice:compacted) | 1 | 1 | 1 | 1 | 63,841 | 63,488 | 0 | 353 | 20 | no price (+1 req no price) | 0 |

Top agents: claude:unknown 52, codex:qa 16, claude:reviewer 15, claude:ui 13, codex:content 12, claude:designer 10, claude:scheduler 8, codex:designer 8.

## reminder_turn

turns opened by "[crew] Reminder: task" (idle reminder); wasted = ended with no tool call or only agent_status; Claude sub-kinds say whether a background Bash/Agent/Monitor launch was still un-notified at that moment.

| Slice | Count | Sessions | Agents | Attributed requests | Input total | Cache read | Cache write | Uncached tail | Output | List cost (input side) | Result bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| total | 61 | 20 | 12 | 865 | 312,925,718 | 309,336,052 | 2,075,907 | 1,513,759 | 687,619 | $68.45 (+230 req no price) | 0 |
| runtime codex | 32 | 10 | 7 | 230 | 51,355,889 | 49,843,456 | 0 | 1,512,433 | 236,116 | no price (+230 req no price) | 0 |
| runtime claude | 29 | 10 | 9 | 635 | 261,569,829 | 259,492,596 | 2,075,907 | 1,326 | 451,503 | $68.45 | 0 |
| kind background_undetectable_codex | 32 | 10 | 7 | 230 | 51,355,889 | 49,843,456 | 0 | 1,512,433 | 236,116 | no price (+230 req no price) | 0 |
| kind no_background_running | 16 | 7 | 6 | 368 | 138,106,350 | 136,941,001 | 1,164,581 | 768 | 205,258 | $36.64 | 0 |
| kind background_running | 13 | 5 | 5 | 267 | 123,463,479 | 122,551,595 | 911,326 | 558 | 246,245 | $31.80 | 0 |

| Runtime | Reminder turns | Ended with no tool call | Only agent_status | Status-only | No model response | Background launch un-notified | …launched within 2h |
|---|---:|---:|---:|---:|---:|---:|---:|
| total | 61 | 2 | 5 | 7 | 14 | 13 | 11 |
| claude | 29 | 2 | 4 | 6 | 0 | 13 | 11 |
| codex | 32 | 0 | 1 | 1 | 14 | not detectable | — |

Wasted reminder turns (no tool call or only agent_status): 21, 12 requests, 4,911,249 input tokens, $0.83.

Top agents: codex:qa 10, claude:scheduler 7, claude:web 6, codex:ui 6, codex:runtime 6, claude:designer 5, claude:qa 5, codex:graph 5.

## agent_inbox calls that returned messages: genuinely new?

| Runtime | Calls with messages | By-id lookups | ≥1 id not delivered in-band | ≥1 id not seen anywhere before | All ids already delivered | Ids returned | New in-band ids |
|---|---:|---:|---:|---:|---:|---:|---:|
| total | 1578 | 46 | 1349 | 1342 | 226 | 5758 | 3818 |
| claude | 104 | 15 | 60 | 59 | 44 | 1562 | 445 |
| codex | 1474 | 31 | 1289 | 1283 | 182 | 4196 | 3373 |

Types of new ids: answer 997, status 785, artifact 708, message 629, question 292, review_result 268, task 88, review_request 49, error 2. Senders: orchestrator 538, scheduler 462, reviewer 398, content 398, qa 383, native-architect 327, graph 274, designer 235, native-transport 198, runtime 183.

## Read result sizes (bytes)

| Tool / runtime | Calls | Total | Min | p50 | p90 | Max | Mean | >50 KB | >100 KB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| agent_inbox all | 2,509 | 3,582,232 | 16 | 525 | 3,292 | 46,648 | 1,428 | 0 | 0 |
| agent_inbox claude | 142 | 1,053,438 | 16 | 1,800 | 26,194 | 46,648 | 7,419 | 0 | 0 |
| agent_inbox codex | 2,367 | 2,528,794 | 16 | 514 | 2,868 | 29,921 | 1,068 | 0 | 0 |
| agents_list all | 108 | 183,000 | 68 | 1,627 | 2,512 | 2,858 | 1,694 | 0 | 0 |
| agents_list claude | 58 | 96,651 | 68 | 1,591 | 2,401 | 2,602 | 1,666 | 0 | 0 |
| agents_list codex | 50 | 86,349 | 138 | 1,701 | 2,629 | 2,858 | 1,727 | 0 | 0 |
| git_diff all | 27 | 449,084 | 11 | 12,249 | 49,173 | 49,173 | 16,633 | 0 | 0 |
| git_diff claude | 2 | 83,870 | 34,697 | 49,173 | 49,173 | 49,173 | 41,935 | 0 | 0 |
| git_diff codex | 25 | 365,214 | 11 | 9,591 | 40,547 | 49,173 | 14,609 | 0 | 0 |
| git_status all | 367 | 70,598 | 70 | 122 | 355 | 2,498 | 192 | 0 | 0 |
| git_status claude | 23 | 3,105 | 94 | 130 | 148 | 224 | 135 | 0 | 0 |
| git_status codex | 344 | 67,493 | 70 | 122 | 365 | 2,498 | 196 | 0 | 0 |
| task_get all | 824 | 2,307,387 | 25 | 2,270 | 5,474 | 16,251 | 2,800 | 0 | 0 |
| task_get claude | 268 | 830,194 | 25 | 2,318 | 6,851 | 16,251 | 3,098 | 0 | 0 |
| task_get codex | 556 | 1,477,193 | 27 | 2,237 | 5,282 | 12,907 | 2,657 | 0 | 0 |
| tasks_list all | 133 | 1,654,464 | 11 | 5,267 | 23,439 | 98,690 | 12,440 | 9 | 0 |
| tasks_list claude | 87 | 621,776 | 11 | 6,166 | 15,861 | 45,743 | 7,147 | 0 | 0 |
| tasks_list codex | 46 | 1,032,688 | 11 | 4,395 | 90,522 | 98,690 | 22,450 | 9 | 0 |
| usage_status all | 11 | 1,858 | 122 | 124 | 172 | 475 | 169 | 0 | 0 |
| usage_status claude | 11 | 1,858 | 122 | 124 | 172 | 475 | 169 | 0 | 0 |

Largest tasks_list results: ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-02-01a1199f-2456-71d2-998c-d6c1de01b676.jsonl:899 98,690 B; ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-00-01a1199f-1c2c-7c40-a73f-8fd5256ebdf1.jsonl:31 90,522 B; ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-02-01a1199f-2456-71d2-998c-d6c1de01b676.jsonl:20 90,522 B; ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-02-01a1199f-2456-71d2-998c-d6c1de01b676.jsonl:37 90,522 B; ~/.codex/sessions/2026/10/07/rollout-2026-10-07T22-47-05-01a1199f-2f0e-74a2-8767-5e2183b1230c.jsonl:29 90,522 B.

## Turns by input class (text-only = model replied without any tool call)

| Runtime + input class | Turns | Text-only | Status-only | No model response | Text-only requests | Text-only input tokens | Text-only list cost |
|---|---:|---:|---:|---:|---:|---:|---:|
| codex message:answer | 838 | 53 | 189 | 356 | 53 | 10,098,568 | no price |
| codex message:artifact | 633 | 40 | 133 | 289 | 40 | 8,672,419 | no price |
| claude non_crew | 598 | 131 | 75 | 10 | 131 | 55,408,529 | $62.19 |
| codex message:message | 436 | 52 | 110 | 138 | 52 | 8,169,657 | no price |
| codex message:status | 399 | 26 | 93 | 186 | 26 | 4,857,170 | no price |
| codex message:question | 237 | 12 | 18 | 102 | 12 | 2,676,957 | no price |
| codex message:review_result | 198 | 10 | 45 | 72 | 9 | 1,773,213 | no price |
| claude batch | 161 | 43 | 38 | 1 | 43 | 17,169,797 | $21.09 |
| codex notice:no_tool_call | 153 | 0 | 53 | 55 | 0 | 0 | no price |
| codex notice:usage_lifted | 142 | 0 | 0 | 113 | 0 | 0 | no price |
| claude notice:fast_forward | 120 | 56 | 58 | 0 | 56 | 26,446,462 | $36.54 |
| claude message:message | 119 | 34 | 33 | 7 | 35 | 15,328,064 | $25.09 |
| claude message:answer | 116 | 72 | 70 | 0 | 72 | 27,738,452 | $69.62 |
| claude message:artifact | 106 | 37 | 34 | 3 | 37 | 13,268,658 | $10.67 |
| claude message:question | 96 | 49 | 29 | 0 | 49 | 14,579,260 | $32.29 |
| codex batch | 92 | 6 | 15 | 17 | 6 | 416,882 | no price |
| claude message:review_result | 86 | 20 | 26 | 0 | 20 | 7,842,423 | $23.57 |
| claude message:review_request | 80 | 1 | 2 | 0 | 1 | 475,852 | $0.12 |
| codex message:task | 74 | 0 | 0 | 19 | 0 | 0 | no price |
| codex message:review_request | 65 | 0 | 3 | 0 | 0 | 0 | no price |
| claude notice:unresponsive | 57 | 31 | 30 | 0 | 31 | 8,310,727 | $1.71 |
| claude notice:team_messages_came | 55 | 8 | 13 | 0 | 8 | 3,925,905 | $14.35 |
| claude message:task | 54 | 0 | 0 | 0 | 0 | 0 | no price |
| claude notice:task_event | 49 | 20 | 7 | 0 | 20 | 4,381,720 | $1.70 |
| claude message:status | 38 | 14 | 13 | 1 | 14 | 7,571,193 | $21.24 |
| codex notice:fast_forward | 33 | 6 | 13 | 12 | 6 | 2,015,883 | no price |
| claude batch:status | 32 | 2 | 1 | 0 | 2 | 428,408 | $0.13 |
| codex notice:task_reminder | 32 | 0 | 1 | 14 | 0 | 0 | no price |
| codex notice:online_idle | 30 | 30 | 30 | 0 | 30 | 751,643 | no price |
| claude notice:task_reminder | 29 | 2 | 6 | 0 | 2 | 780,167 | $0.16 |

Failed turns (native turn error, usually no model response): codex Your workspace is out of credits. Ask your workspace owner to refill i ×806; codex Your workspace is out of credits. Add credits to continue. ×472; codex exceeded retry limit, last status: N Too Many Requests, request id: <s ×148; codex Error running remote compact task: Your workspace is out of credits. A ×26; codex Selected model is at capacity. Please try a different model. ×5; codex This content was flagged for possible cybersecurity risk. If this seem ×3.

## Crew input openings (top 20, all [crew] inputs)

| Opening | Class | Count | Turn start | Folded | Claude | Codex |
|---|---|---:|---:|---:|---:|---:|
| [crew] Message from <agent> (type answer) | message:answer | 1186 | 954 | 232 | 347 | 839 |
| [crew] Message from <agent> (type artifact) | message:artifact | 932 | 739 | 193 | 299 | 633 |
| [crew] Message from <agent> (type message) | message:message | 813 | 555 | 258 | 377 | 436 |
| [crew] Message from <agent> (type status) | message:status | 587 | 437 | 150 | 188 | 399 |
| [crew] Message from <agent> (type review_result) | message:review_result | 422 | 284 | 138 | 224 | 198 |
| [crew] Message from <agent> (type question) | message:question | 404 | 333 | 71 | 167 | 237 |
| [crew] N new messages: | batch | 294 | 290 | 4 | 196 | 98 |
| [crew] Message from <agent> (type review_request) | message:review_request | 278 | 145 | 133 | 213 | 65 |
| [crew] Message from <agent> (type status): [crew] Your last turn ended without a tool call | notice:no_tool_call | 190 | 154 | 36 | 36 | 154 |
| [crew] Message from <agent> (type message): main fast-forwarded to <sha> | notice:fast_forward | 176 | 97 | 79 | 155 | 21 |
| [crew] Message from <agent> (type task) | message:task | 164 | 128 | 36 | 88 | 76 |
| [crew] The usage limit that stopped you has lifted. Continue your current task where you l | notice:usage_lifted | 142 | 142 | 0 | 0 | 142 |
| [crew] Message from <agent> (type status): Task <id> (…) was completed by <agent> | notice:task_event | 64 | 60 | 4 | 64 | 0 |
| [crew] Reminder: task "…" (id …) is still open. Continue working on it; call task_complete | notice:task_reminder | 61 | 61 | 0 | 29 | 32 |
| [crew] Message from <agent> (type status): Agent <agent> is unresponsive | notice:unresponsive | 58 | 57 | 1 | 58 | 0 |
| [crew] You are online as <agent>. Nothing is assigned to you yet. Reply with one short lin | notice:online_idle | 57 | 57 | 0 | 27 | 30 |
| [crew] Team messages came as your last turn ended (above). Act on them. | notice:team_messages_came | 55 | 55 | 0 | 55 | 0 |
| [crew] Message from <agent> (type status): Your conversation was just compacted | notice:compacted | 38 | 37 | 1 | 13 | 25 |
| [crew] Message from <agent> (type status): Card <id> revised by orchestrator | notice:card_revised | 36 | 28 | 8 | 8 | 28 |
| [crew] Message from <agent> (type status): <agent>'s model | notice:refusal | 17 | 14 | 3 | 17 | 0 |

## No-action notice openings (top 20)

| Opening | Class | Count | Turn start | Folded | Top senders |
|---|---|---:|---:|---:|---|
| [crew] Message from <agent> (type status) | message:status | 587 | 437 | 150 | qa 126, orchestrator 82, scheduler 55 |
| [crew] Message from <agent> (type status): [crew] Your last turn ended without a tool call | notice:no_tool_call | 190 | 154 | 36 | scheduler 190 |
| [crew] Message from <agent> (type message): main fast-forwarded to <sha> | notice:fast_forward | 176 | 97 | 79 | orchestrator 176 |
| [crew] The usage limit that stopped you has lifted. Continue your current task where you l | notice:usage_lifted | 142 | 142 | 0 |  |
| [crew] Message from <agent> (type status): Task <id> (…) was completed by <agent> | notice:task_event | 64 | 60 | 4 | scheduler 64 |
| [crew] Reminder: task "…" (id …) is still open. Continue working on it; call task_complete | notice:task_reminder | 61 | 61 | 0 |  |
| [crew] Message from <agent> (type status): Agent <agent> is unresponsive | notice:unresponsive | 58 | 57 | 1 | scheduler 58 |
| [crew] You are online as <agent>. Nothing is assigned to you yet. Reply with one short lin | notice:online_idle | 57 | 57 | 0 |  |
| [crew] Team messages came as your last turn ended (above). Act on them. | notice:team_messages_came | 55 | 55 | 0 |  |
| [crew] Message from <agent> (type status): Your conversation was just compacted | notice:compacted | 38 | 37 | 1 | scheduler 38 |
| [crew] Message from <agent> (type status): Card <id> revised by orchestrator | notice:card_revised | 36 | 28 | 8 | scheduler 36 |
| [crew] Message from <agent> (type status): <agent>'s model | notice:refusal | 17 | 14 | 3 | scheduler 17 |
| [crew] Message from <agent> (type status): <agent> cannot act on your message m-X | notice:refusal | 12 | 3 | 9 | scheduler 12 |
| [crew] Message from <agent> (type message): Lead change: orchestrator now Claude Code/Opus | notice:fast_forward | 8 | 6 | 2 | orchestrator 8 |
| [crew] Message from <agent> (type message): Correction: reviewer confirmed approval covers graph chain to <sha> | notice:fast_forward | 8 | 3 | 5 | orchestrator 8 |
| [crew] Message from <agent> (type status): Agent qa (Codex) hit a usage limit | notice:usage_limit | 3 | 2 | 1 | scheduler 3 |
| [crew] Message from <agent> (type message): native-linux-graphs approved | notice:fast_forward | 3 | 0 | 3 | graph 2, orchestrator 1 |
| [crew] Fable week has reset: your session was restarted on Fable N, your own model again.  | notice:model_restored | 2 | 2 | 0 |  |
| [crew] Message from <agent> (type status): All N tasks are done | notice:all_done | 2 | 1 | 1 | scheduler 2 |
| [crew] Message from <agent> (type status): Agent designer | notice:usage_limit | 2 | 1 | 1 | scheduler 2 |

## Limits

- Attribution is heuristic. A wasted call (inbox_empty, inbox_duplicate_only, inbox_after_delivery, repeated_read) is "alone" when every tool call consumed by the same next model response is agent_inbox/agent_status/task_update; it is then charged that whole consuming request (deduplicated per pattern). A call bundled with other real tool work (Codex exec with exec_command etc., Claude parallel tool_use) is charged its result bytes only. The issuing response is never charged. delivery_round_trips charges consuming requests split equally among sibling calls. Turn patterns are charged every request in the turn.
- Background work at reminder time is Claude-only: Bash run_in_background, async Agent or Monitor launches confirmed by their tool result and not yet named by a <task-notification>. A process restart can orphan a launch, so the un-notified count is an upper bound; the 2h column limits it to recent launches. Codex background terminals are not detectable from the rollout.
- Inbox novelty compares returned message ids with ids delivered in-band earlier in the same session file; a session that began after an earlier delivery (restart, takeover) can make an old id look new.
- Claude requests are deduplicated by message id with the last streamed usage winning (baseline.mjs). Codex requests are token_count cumulative deltas per sampling; a Codex inner tool call (crew MCP call inside an `exec` code-mode wrapper) returns with its wrapper output, so all inner calls of one wrapper share the consuming request.
- Codex model responses are delimited by output items following an input or tool output; when two exec wrappers run in one sampling, inner calls are linked to the first wrapper output after them.
- List cost is input side only (uncached tail × base, cache writes × 5m/1h write price, cache reads × read price) from notes/prompt-cache-audit/prices.tsv; output tokens are not priced. Codex models (gpt-*) have no listed price and are reported as "no price".
- Patterns overlap (an empty inbox call after a delivery appears in inbox_empty and inbox_after_delivery; a notice turn may also be a status-only turn). Do not add pattern totals.
- Status-only/no-action is inferred from tool names and agent_send type only; a status message can still carry information the team needed. Short reply = total assistant text in the turn ≤ the threshold, with no tool calls.
- Repeated-read detection resets on any own crew mutating call (task_*, review_*, git_commit/merge/cherry_pick, agent_status, agent lifecycle) and, for git_status/git_diff, on any own file-modifying tool call. Other agents may have changed state invisibly; identical bytes mean the read returned nothing new.
- Mid-turn folded crew messages (Claude isMeta, Codex inputs after model output in the same task) count as deliveries for the inbox window and openings, but are not turns and are not charged turn tokens.
- Sessions are read up to the byte size baseline.mjs read; later appends are excluded.
