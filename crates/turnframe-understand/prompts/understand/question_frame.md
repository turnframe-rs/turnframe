Say what the user's question is about.

topic is what kind of thing it asks:
- record_state: what a record holds or where it stands, and where the work in progress stands: a field's value, whether something happened, what is being done, what is left to do.
- accepted_values: which values a field takes. Right after the assistant asked for a field, a question about the options or choices the user has asks for that field's values, and that field is among its subjects.
- capabilities: what the user can do here, in general.
- ability: whether one particular thing the user names can be done («can I set A?», in any language): that asks for it to be done.
- knowledge: general knowledge of the domain, about none of the records here, such as what a term means.

record is the record the question is about, by its handle, or none when it is about no record in particular. A question about the work in progress is about the record the last assistant message was about, else the one that still needs something.

subjects are the listed subjects the question asks about, for record_state and accepted_values only; give none otherwise, and none that the question does not name.
