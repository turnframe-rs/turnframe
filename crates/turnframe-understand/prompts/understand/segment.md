Split the user's message into units. A unit is one thing the message does:

- request: asks for something to be done, or states what a record holds («the A is X»), even that a value cannot be given. Each thing to do is its own unit: «set A to X and B to Y» is two, and so is «no, A is X, and B is Y».
- question: asks for information. Asking whether one particular thing can be done («can I set …?», «posso impostare …?», in any language) is a request for it; asking what can be done is a question. Its basis: current_committed_state for what is saved now, proposed_state for a change not saved yet, committed_state_after_turn for after this message is carried out, general_domain_knowledge for anything not about a record. continues_previous is true when it follows up the assistant's last message.
- constraint: a condition on the whole message, such as not yet, or only if something holds. Asking to leave something as it is («don't touch A», «non toccare A») is keep_unchanged.
- correction: changes something asked for earlier, in the words that change it alone; what the message goes on to give or ask is its own unit. corrects is the number of the unit it changes in your list, or null when it changes something from an earlier turn.
- cancel: withdraws something asked for earlier: cancels is the unit number, or null for an earlier turn.
- card_answer: answers the card on screen with one of its options.
- dispute: says a change the assistant reported is wrong; receipt names it.
- provides_value: gives what the assistant asked for, however short, with any word beside it that adds nothing, and nothing else; never a question. Asking to create a record is a request, even with a value.
- chitchat: greets, thanks, or asks for nothing.

Units are numbered from 1 in the order you list them. Point at each unit's words by their bracketed numbers, first to last. Units never share a word. Give each unit the workflow it is about, or unknown.

Write analysis first: one or two sentences on what the message asks for, in order.
