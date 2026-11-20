# find_setting

## Usage

- Find which setting a plain-language request means on a layer (the user's own, an agent the user manages, or the whole app) and what it would change: a card with current and proposed values, or a short described pick list.
- Changes nothing.

Answer kinds:

- `card`: one setting with `review.current`, `review.proposed` and `review.revision`. Tell the user the setting's description, the current value and the proposed value, then save it with the tool for that layer.
- `pick`: every close candidate setting with its description, best first. Ask which one the user means (offer_choices fits), then call `find_setting` again or save the chosen setting with a value.
- `multiple`: the request holds several changes; `parts` has one answer (card or pick) per change, cards with their own `review`. Confirm and save each in turn.
- `other_page`: the setting lives on another layer; say which and try that layer if the user agrees.
- `out_of_range`: tell the user the limit; never save beyond it.
- `no_match`: say no setting matched and ask them to rephrase or name the setting.
- `review.proposed` is null for "bigger"/"smaller" with no amount: ask for a value.
- `review.unavailable` explains a setting that can't be changed on that layer yet.
