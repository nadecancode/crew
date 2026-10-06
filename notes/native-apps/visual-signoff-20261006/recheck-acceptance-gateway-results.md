# Linux acceptance: gateway suite

| Case | Rows | Result | Checks | Notes |
| --- | --- | --- | --- | --- |
| C01 Connections without a running gateway | N01 C01 | pass | 4/4 |  |
| C02 Public profile form and errors | N01 C02 | pass | 7/7 |  |
| C03 Fingerprint mismatch and trust | N01 C03 | pass | 6/6 |  |
| C12 Device name and key storage | N01 C12 | pass | 5/5 |  |
| C04 No host enrollment window | N01 C04 | pass | 2/2 |  |
| C05 Comparison code matches the host console | N01 C05 | pass | 6/6 |  |
| C10 Mismatched code at the host approves nothing | N01 C10 | pass | 5/5 |  |
| C06 Host-selected Read grant | N01 N02 C06 C09 | pass | 4/4 |  |
| C08 Access disclosure and details | N01 C08 C18 | pass | 5/5 |  |
| C07 Read-only device: unavailable actions | N08 N11 C07 | pass | 7/7 |  |
| C15 Host adds Control; old authority retires | N09 C15 | pass | 4/4 |  |
| C14 Gateway restart with retained grants | N13 C14 | pass | 5/5 |  |
| C16 Host revocation | N13 C16 | pass | 6/6 |  |
| C18 Local removal copy | N13 C18 | pass | 1/1 |  |
