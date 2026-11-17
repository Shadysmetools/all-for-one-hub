# SPDX-License-Identifier: Apache-2.0
"""Client-facing message templates, English and Arabic, rendered from configuration.

Three deliberate message kinds:
  - ack     : instant acknowledgement (the ONLY automatic client message)
  - confirm : confirm understanding, optionally with open questions (owner-approved before sending)
  - resolve : resolution, optionally with a short proof video (owner-approved before sending)

The defaults below are simple generic examples for a fictional company. A deployment overrides any of them under
the ``templates:`` key of its config (see ``config.example.yaml``). Rendering only fills ``{placeholders}``; it
never translates. The reply language follows the sender (``model.detect_lang``).
"""
from __future__ import annotations

from typing import Any

DEFAULT_COMPANY = "Northwind Supplies"

DEFAULTS: dict[str, dict[str, str]] = {
    "sign": {"en": "— Northwind Supplies Support", "ar": "— دعم Northwind"},
    "ack": {
        "en": "Hello {name}, thank you for reaching out. We've logged this as ticket #{n}, and {owner} is now "
              "handling it. Next, we'll {next_step} and update you here.",
        "ar": "مرحباً {name}، شكراً لتواصلك معنا. سجّلنا طلبك كتذكرة رقم #{n}، و{owner} يتابعه الآن. "
              "الخطوة التالية: {next_step}، وسنوافيك بالتحديثات هنا."},
    "next_step_questions": {"en": "confirm a few details with you", "ar": "نؤكد معك بعض التفاصيل"},
    "next_step_plain": {"en": "confirm our understanding and proceed", "ar": "نؤكد فهمنا للمطلوب ونتابع"},
    "confirm_core": {
        "en": "Hello {name}, to make sure we solve the right thing: you're seeing {issue} on {module}, and you'd "
              "like {outcome}. Is that correct? Once you confirm, we'll proceed.",
        "ar": "مرحباً {name}، للتأكد من أننا نعالج النقطة الصحيحة: تواجه {issue} في {module}، وتريد {outcome}. "
              "هل هذا صحيح؟ بمجرد التأكيد سنتابع."},
    "confirm_questions": {
        "en": "Before we start, we need a few things from you: {items}. Once you confirm the above and answer "
              "these, we'll begin.",
        "ar": "قبل أن نبدأ، نحتاج منك بعض المعلومات: {items}. بعد التأكيد والإجابة سنبدأ."},
    "resolve_core": {
        "en": "Hello {name}, your request (ticket #{n}) is resolved: {what}. You can check it here: {link}. If "
              "anything still looks off, just reply and we'll reopen it; otherwise we'll close it. Thank you for "
              "your patience.",
        "ar": "مرحباً {name}، تم حل طلبك (تذكرة #{n}): {what}. يمكنك التحقق من هنا: {link}. إن كان هناك ما لا "
              "يزال غير صحيح فرد علينا وسنعيد فتحها، وإلا سنغلقها. شكراً لصبرك."},
    "resolve_video": {"en": "Here's a short video showing it working: {video}. {verify}.",
                      "ar": "هذا فيديو قصير يوضح أنها تعمل: {video}. {verify}."},
    "verify_step": {"en": "Open the video and verify the step", "ar": "افتح الفيديو وتحقق من الخطوة"},
}

DEFAULT_MODULES_AR = {"Inventory": "المخزون", "Sales": "المبيعات", "Purchase": "المشتريات",
                      "Accounting": "المحاسبة", "POS": "نقطة البيع", "Manufacturing": "التصنيع",
                      "HR": "الموارد البشرية", "Helpdesk": "الدعم", "Website": "الموقع", "CRM": "العملاء المحتملون",
                      "unknown": "غير محدد"}

# Arabic-Indic digits for the AR numbered open-questions list.
_AR_DIGITS = {"0": "٠", "1": "١", "2": "٢", "3": "٣", "4": "٤", "5": "٥", "6": "٦", "7": "٧", "8": "٨", "9": "٩"}


def _ar_num(n: int) -> str:
    return "".join(_AR_DIGITS.get(c, c) for c in str(n))


def _lang(lang: str) -> str:
    return "ar" if str(lang).lower().startswith("ar") else "en"


class Templates:
    """Renders the message kinds from DEFAULTS overridden by the config's ``templates`` mapping."""

    def __init__(self, overrides: dict[str, Any] | None = None, modules_ar: dict[str, str] | None = None):
        ov = overrides or {}
        self.t = {k: {**v, **{lg: str(s) for lg, s in (ov.get(k) or {}).items() if lg in ("en", "ar")}}
                  for k, v in DEFAULTS.items()}
        self.modules_ar = {**DEFAULT_MODULES_AR, **(modules_ar or {})}

    @classmethod
    def from_config(cls, cfg: dict | None) -> "Templates":
        c = cfg or {}
        return cls(c.get("templates"), (c.get("module_labels") or {}).get("ar"))

    def sign(self, lang: str) -> str:
        return self.t["sign"][_lang(lang)]

    def sign_markers(self) -> tuple[str, ...]:
        return tuple(self.t["sign"].values())

    def module_label(self, lang: str, module: str) -> str:
        """The module name in the client's language (Arabic messages must not carry English fragments)."""
        if _lang(lang) == "ar":
            return self.modules_ar.get(module, module)
        return module

    def next_step(self, lang: str, *, has_questions: bool) -> str:
        return self.t["next_step_questions" if has_questions else "next_step_plain"][_lang(lang)]

    def ack(self, lang: str, *, name: str, n: int, owner: str, next_step: str) -> str:
        body = self.t["ack"][_lang(lang)].format(name=name, n=n, owner=owner, next_step=next_step)
        return f"{body} {self.sign(lang)}"

    def _confirm_core(self, lang: str, *, name: str, issue: str, module: str, outcome: str) -> str:
        return self.t["confirm_core"][_lang(lang)].format(
            name=name, issue=issue, module=self.module_label(lang, module), outcome=outcome)

    def confirm(self, lang: str, *, name: str, issue: str, module: str, outcome: str,
                questions: list[str] | None = None) -> str:
        """Confirm understanding (no questions) or with open questions. Owner-approved before sending."""
        core = self._confirm_core(lang, name=name, issue=issue, module=module, outcome=outcome)
        qs = [q for q in (questions or []) if str(q).strip()]
        if not qs:
            return f"{core} {self.sign(lang)}"
        if _lang(lang) == "ar":
            items = "؛ ".join(f"{_ar_num(i)}) {q}" for i, q in enumerate(qs, 1))
        else:
            items = "; ".join(f"{i}) {q}" for i, q in enumerate(qs, 1))
        tail = self.t["confirm_questions"][_lang(lang)].format(items=items)
        return f"{core} {tail} {self.sign(lang)}"

    def resolve(self, lang: str, *, name: str, n: int, what: str, link: str, video: str | None = None,
                verify_step: str = "") -> str:
        """Resolution (no video) or resolution with a short proof clip. Owner-approved before sending."""
        core = self.t["resolve_core"][_lang(lang)].format(name=name, n=n, what=what, link=link)
        if not video:
            return f"{core} {self.sign(lang)}"
        verify = verify_step or self.t["verify_step"][_lang(lang)]
        tail = self.t["resolve_video"][_lang(lang)].format(video=video, verify=verify)
        return f"{core} {tail} {self.sign(lang)}"


_DEFAULT = Templates()

# Module-level helpers render with the built-in defaults (handy in tests and scripts).
sign_markers = _DEFAULT.sign_markers
module_label = _DEFAULT.module_label
next_step = _DEFAULT.next_step
ack = _DEFAULT.ack
confirm = _DEFAULT.confirm
resolve = _DEFAULT.resolve
