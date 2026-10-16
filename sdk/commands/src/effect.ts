import { Context, Effect, Schema } from "effect"
import {
  openApplication,
  openPath,
  openUrl,
  press,
  PROTOCOL_VERSION,
  typeText,
  choice,
  digit,
  letter,
  text,
  union,
} from "./model.js"
import type {
  CapturesFor,
  CommandDefinitionFor,
  CaptureSchema,
  CommandMetadata,
  HandlerContext,
  HelloCapabilities,
  HelloConfigFor,
  NativeAction,
  UntypedCaptures,
} from "./model.js"

export class ToolCallError extends Schema.TaggedError<ToolCallError>()(
  "Hello.ToolCallError",
  { message: Schema.String, code: Schema.optionalKey(Schema.String) },
) {}

export interface HelloService extends HelloCapabilities<Effect.Effect<void, ToolCallError>> {
  readonly context: HandlerContext
  readonly captures: UntypedCaptures
}

export class Hello extends Context.Service<Hello, HelloService>()("@hello/commands/Hello") {}

export interface EffectHandlerArguments<Captures extends UntypedCaptures = Readonly<Record<string, string>>> {
  readonly context: HandlerContext
  readonly captures: Captures
}

export type EffectHandler<E = unknown> =
  | Effect.Effect<void, E, Hello>
  | ((arguments_: EffectHandlerArguments) => Effect.Effect<void, E, Hello>)

export type EffectCommandDefinition = CommandDefinitionFor<EffectHandler>

export interface EffectHelloConfig extends HelloConfigFor<EffectCommandDefinition> {}

type EffectCommandFor<Schema> = CommandMetadata
  & { readonly captures?: Schema & CaptureSchema }
  & (
    | {
      readonly action: [Schema] extends [never]
        ? NativeAction
        : [Schema] extends [CaptureSchema] ? never : NativeAction
      readonly run?: never
    }
    | {
      readonly action?: never
      readonly run: [Schema] extends [CaptureSchema]
        ? Effect.Effect<void, unknown, Hello>
          | ((arguments_: EffectHandlerArguments<CapturesFor<Schema>>) => Effect.Effect<void, unknown, Hello>)
        : NativeAction
          | Effect.Effect<void, unknown, Hello>
          | ((arguments_: EffectHandlerArguments<CapturesFor<Schema>>) => Effect.Effect<void, unknown, Hello>)
    }
  )

export type EffectTypedCommandDefinition<Schema extends CaptureSchema> = EffectCommandFor<Schema>

export const defineHelloConfig = <
  const Captures extends Readonly<Record<string, unknown>>,
  const Extra extends Omit<EffectHelloConfig, "commands">,
>(config: Extra & {
  readonly commands: { readonly [Id in keyof Captures]: EffectCommandFor<Captures[Id]> }
}): typeof config => config

export {
  openApplication,
  openPath,
  openUrl,
  press,
  PROTOCOL_VERSION,
  typeText,
  choice,
  digit,
  letter,
  text,
  union,
}
export type {
  CommandMetadata,
  DictationProtocolConfig,
  HandlerContext,
  HelloConfig,
  Modifier,
  NativeAction,
  PressOptions,
  Transformation,
  TransformationDefinition,
  CaptureDescriptor,
  CaptureSchema,
  CaptureValues,
  ChoiceCapture,
  DigitCapture,
  Letter,
  LetterCapture,
  TextCapture,
  AtomicCaptureDescriptor,
  BoundedCaptureDescriptor,
  UnionCapture,
} from "./model.js"
