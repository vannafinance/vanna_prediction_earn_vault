# YES/NO Merge `PrivilegeEscalation` Fix

## Kya issue tha?

`Merge Now` par pool ke YES aur NO tokens pehle World `prediCt` program mein merge hokar CASH bante the. Live transaction simulation mein yeh step successful tha. Agle step mein Jupiter ke `Route` CPI ko pool PDA read-only mil raha tha, lekin Jupiter usi pool ko writable transfer authority ke roop mein apni nested CPI mein aage bhejta hai. Solana is privilege ko escalate karne se rokta hai, isliye transaction `PrivilegeEscalation` se fail hoti thi.

## Kya badla?

`programs/outcome_yield_pool/src/jupiter.rs` mein Jupiter CPI ke account metas ab Jupiter ke diye writable flags preserve karte hain. Pool PDA outer instruction mein pehle se writable hota hai, aur program uski PDA signature seeds se deta hai. Ab Jupiter ko route ke andar usi writable privilege ko aage pass karne ki permission milti hai.

Frontend `lib/world/pool.ts` ke purane comments bhi update kiye, taaki woh writable privilege ke baare mein galat explanation na dein.

## Sab markets par kyun?

Har market apne pool PDA ka use karta hai, lekin sab merge ek hi `merge_and_deposit` instruction aur Jupiter CPI code se chalate hain. Isliye fix common merge path par hai; kisi ek market ke mint ya pool address ke liye special case nahi hai.

## Checks aur live status

- Deposit/merge integration tests: 15 pass.
- Multi-pool, three-market tests: 9 pass.
- Jupiter swap guard tests: 19 pass.
- Updated SBF program build: `target/deploy/outcome_yield_pool.so`.
- Live Seattle pool ki read-only simulation mein World merge pass hua aur Jupiter CPI par writable privilege failure reproduce hui.
- Updated program abhi fork par deploy nahi hua hai; live RPC par fix aane ke liye deployed program upgrade karna baaki hai.
