//! Verified FlatBuffers images. Borrowed getters require no payload deserialization.
use crate::{Error, Record, Result, Scalar, actor_generated::ciagomem::image as fb};
use flatbuffers::FlatBufferBuilder;
pub fn encode(r: &Record) -> Vec<u8> {
    let mut b = FlatBufferBuilder::new();
    let mut fields = Vec::with_capacity(r.fields.len());
    for (name, value) in &r.fields {
        let name = Some(b.create_string(name));
        let text = if let Scalar::Text(s) = value {
            Some(b.create_string(s))
        } else {
            None
        };
        let mut args = fb::FieldArgs {
            name,
            text_value: text,
            ..Default::default()
        };
        match value {
            Scalar::Null => args.kind = fb::Kind::Null,
            Scalar::Bool(v) => {
                args.kind = fb::Kind::Bool;
                args.bool_value = *v
            }
            Scalar::Int(v) => {
                args.kind = fb::Kind::Int;
                args.int_value = *v
            }
            Scalar::Float(v) => {
                args.kind = fb::Kind::Float;
                args.float_value = *v
            }
            Scalar::Text(_) => args.kind = fb::Kind::Text,
            Scalar::Ref(v) => {
                args.kind = fb::Kind::Ref;
                args.ref_id = v.id;
                args.ref_inc_lo = v.incarnation as u64;
                args.ref_inc_hi = (v.incarnation >> 64) as u64
            }
        }
        fields.push(fb::Field::create(&mut b, &args));
    }
    let fields = Some(b.create_vector(&fields));
    let lo: Vec<_> = r.label.readers().iter().map(|x| *x as u64).collect();
    let hi: Vec<_> = r
        .label
        .readers()
        .iter()
        .map(|x| (*x >> 64) as u64)
        .collect();
    let reader_lo = Some(b.create_vector(&lo));
    let reader_hi = Some(b.create_vector(&hi));
    let embedding = Some(b.create_vector(&r.embedding));
    let root = fb::ActorImage::create(
        &mut b,
        &fb::ActorImageArgs {
            id: r.actor.id,
            incarnation_lo: r.actor.incarnation as u64,
            incarnation_hi: (r.actor.incarnation >> 64) as u64,
            tenant_lo: r.tenant as u64,
            tenant_hi: (r.tenant >> 64) as u64,
            schema_version: r.schema_version,
            fields,
            integrity: r.label.integrity() as u8,
            sources: r.label.sources(),
            reader_lo,
            reader_hi,
            embedding,
        },
    );
    fb::finish_actor_image_buffer(&mut b, root);
    b.finished_data().to_vec()
}
pub fn view(bytes: &[u8]) -> Result<fb::ActorImage<'_>> {
    if bytes.len() < 8 || !fb::actor_image_buffer_has_identifier(bytes) {
        return Err(Error::InvalidImage);
    }
    let v = fb::root_as_actor_image(bytes).map_err(|_| Error::InvalidImage)?;
    if v.reader_lo().len() != v.reader_hi().len() || v.reader_lo().len() > 4 {
        return Err(Error::InvalidImage);
    }
    Ok(v)
}
