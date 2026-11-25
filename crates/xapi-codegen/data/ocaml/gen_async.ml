(* Emits, for the same client-visible API that gen_json documents, the
   messages that have an Async.* variant (msg_async = true), as
   {"<class>": ["<message>", ...], ...}. gen_json does not export this flag. *)
open Datamodel_types
open Datamodel_utils
open Dm_api

let () =
  let api = Datamodel.all_api in
  let api = add_implicit_messages api in
  let api = filter_by ~message:on_client_side api in
  let api =
    filter_by
      ~field:(fun f -> not f.internal_only)
      ~message:(fun m -> not m.msg_hide_from_docs)
      api
  in
  let objs = objects_of_api api in
  let json =
    `Assoc
      (List.map
         (fun obj ->
           ( obj.name
           , `List
               (List.filter_map
                  (fun m -> if m.msg_async then Some (`String m.msg_name) else None)
                  obj.messages
               )
           )
         )
         objs
      )
  in
  Yojson.Safe.to_file Sys.argv.(1) json
