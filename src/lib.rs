#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_mut)]
extern crate rand;
//extern crate bn;
//use bn::{Group, Fr, G1, G2, pairing};
use rand::RngCore;
use bls12_381 as bls;
use sha2::{Sha256, Sha512, Digest};
use hex_slice::AsHex;
use std::iter::Sum;

// Use this for handling Sha2 generic arrays and from_bytes/compressed for scalar/GX functions
fn clone_into_array<A, T>(slice: &[T]) -> A
    where A: Sized + Default + AsMut<[T]>,
          T: Clone
{
    let mut a = Default::default();
    <A as AsMut<[T]>>::as_mut(&mut a).clone_from_slice(slice);
    a
}

pub trait MyRandom {
    fn new_random<T: RngCore>(rng: &mut T) -> Self;
}

impl MyRandom for bls::Scalar {
    fn new_random<T: RngCore>(rng: &mut T) -> Self {
        loop {
            let mut random_bytes = [0u8; 32];
            rng.fill_bytes(&mut random_bytes);
            let scalar = bls::Scalar::from_bytes(&random_bytes);
            if scalar.is_some().unwrap_u8() == 1 {
                break scalar.unwrap()
            }
        }
    }
}

pub struct Parameters<'a> {
    g1: bls::G1Affine,
    hs: Vec<bls::G1Affine>,
    g2: bls::G2Affine,
    rng: &'a mut dyn RngCore
}

impl<'a> Parameters<'a> {
    pub fn new<T: RngCore>(attributes_size: usize, rng: &'a mut T) -> Self {
        let g1 = bls::G1Affine::generator();
        let g2 = bls::G2Affine::generator();

        let mut hs = Vec::with_capacity(attributes_size);
        for i in 0 .. attributes_size {
            let message = format!("h{}", i);
            let h = bls::G1Affine::hash_to_point(message.as_bytes());
            hs.push(h);
        }

        Parameters {
            g1: g1,
            hs: hs,
            g2: g2,
            rng: rng
        }
    }

    fn random_scalar(&mut self) -> bls::Scalar {
        bls::Scalar::new_random(&mut self.rng)
    }
}

fn sha256_hash(message: &[u8]) -> [u8; 48] {
    let mut hasher = Sha512::new();
    hasher.input(message);
    let hash_result = hasher.result();
    assert_eq!(hash_result.len(), 64);

    let mut hash_data = [0u8; 48];
    hash_data.copy_from_slice(&hash_result[0..48]);
    //println!("{:x}", hash_data.as_hex());
    hash_data
}

pub trait HashableGenerator {
    fn hash_to_point(message: &[u8]) -> Self;
}

impl HashableGenerator for bls::G1Affine {
    fn hash_to_point(message: &[u8]) -> Self {
        for i in 0u32 .. {
            let i_data = i.to_le_bytes();

            let mut data = Vec::with_capacity(message.len() + i_data.len());
            data.extend_from_slice(message);
            data.extend_from_slice(&i_data);

            let hash = sha256_hash(data.as_slice());

            let point = {
                let point_optional = Self::from_compressed_unchecked(&hash);
                if point_optional.is_none().unwrap_u8() == 1 {
                    continue;
                }
                let affine_point = point_optional.unwrap();
                let projective_point = bls::G1Projective::from(affine_point).clear_cofactor();
                Self::from(projective_point)
            };

            assert_eq!(bool::from(point.is_on_curve()), true);
            assert_eq!(bool::from(point.is_torsion_free()), true);

            return point;
        }
        unreachable!();
    }
}

impl HashableGenerator for bls::G1Projective {
    fn hash_to_point(message: &[u8]) -> Self {
        bls::G1Projective::from(bls::G1Affine::hash_to_point(&message))
    }
}

fn compute_polynomial(coefficients: &Vec<bls::Scalar>, x_primitive: u64)
    -> bls::Scalar
{
    let x = bls::Scalar::from(x_primitive);
    coefficients.iter()
        .enumerate()
        .map(|(i, coefficient)| coefficient * x.pow(&[i as u64, 0, 0, 0]))
        .fold(bls::Scalar::zero(), |result, x| result + x)
}

type ScalarList = Vec<bls::Scalar>;
type PointList = Vec<bls::G2Projective>;
type VerifyKey = (bls::G2Projective, PointList);
type SecretKey = (bls::Scalar, ScalarList);

pub fn ttp_keygen(params: &mut Parameters, threshold: usize, number_authorities: usize) 
    -> (Vec<SecretKey>, Vec<VerifyKey>) {
    let attributes_size = params.hs.len();
    assert!(number_authorities >= threshold && threshold > 0);
    assert!(attributes_size > 0);

    let mut create_n_random_scalars = |n| -> Vec<_> {
        (0..n).map(|_| params.random_scalar()).collect()
    };

    // Generate polynomials
    let v_poly = create_n_random_scalars(threshold);
    let w_poly: Vec<Vec<bls::Scalar>> =
        (0..attributes_size).map(|_| create_n_random_scalars(threshold)).collect();

    // Generate shares
    let x_shares: Vec<bls::Scalar> = (1..number_authorities + 1).map(
        |i| compute_polynomial(&v_poly, i as u64)).collect();
    let y_shares: Vec<Vec<bls::Scalar>> = (1..number_authorities + 1).map(
        |i| w_poly.iter().map(move |w_coefficients|
            compute_polynomial(&w_coefficients, i as u64)).collect()).collect();

    // Set the keys
    // sk_i = (x, (y_1, y_2, ..., y_q))
    // vk_i = (g2^x, (g2^y_1, g2^y_2, ..., g2^y_q)) = (a, (B_1, B_2, ..., B_q))
    let verify_keys: Vec<(bls::G2Projective, Vec<bls::G2Projective>)> =
        x_shares.iter()
            .enumerate()
            .map(
                |(i, x)| (params.g2 * x, y_shares[i].iter().map(|y| params.g2 * y).collect()))
            .collect();
    let secret_keys: Vec<(bls::Scalar, Vec<bls::Scalar>)> =
        x_shares.into_iter().zip(y_shares).collect();

    (secret_keys, verify_keys)
}

fn lagrange_basis(range_len: u64) -> ScalarList {
    let x = bls::Scalar::zero();
    let mut lagrange_result = ScalarList::new();
    for i in 1..=range_len {
        let mut numerator = bls::Scalar::one();
        let mut denominator = bls::Scalar::one();

        for j in 1..=range_len {
            if j == i {
                continue;
            }
            numerator = numerator * (x - bls::Scalar::from(j));
            denominator = denominator * (bls::Scalar::from(i) - bls::Scalar::from(j));
        }

        let result = numerator * denominator.invert().unwrap();
        lagrange_result.push(result);
    }
    lagrange_result
}

pub trait GeneratorPoint {
    fn get_identity() -> Self;
    fn add(&self, rhs: &Self) -> Self;
}

impl GeneratorPoint for bls::G1Projective {
    fn get_identity() -> Self {
        Self::identity()
    }

    fn add(&self, rhs: &Self) -> Self {
        self + rhs
    }
}

impl GeneratorPoint for bls::G2Projective {
    fn get_identity() -> Self {
        Self::identity()
    }

    fn add(&self, rhs: &Self) -> Self {
        self + rhs
    }
}

fn ecc_sum<G: GeneratorPoint + Sized>(points: &Vec<G>) -> G {
    points.iter()
        .fold(G::get_identity(), |result, x| result.add(x))
}

fn ec_sum(points: &Vec<bls::G2Projective>) -> bls::G2Projective {
    points.iter()
        .fold(bls::G2Projective::identity(), |result, x| result + *x)
}

pub fn aggregate_keys(params: &Parameters, verify_keys: &Vec<VerifyKey>)
    -> (bls::G2Projective, PointList) {
    let lagrange = lagrange_basis(verify_keys.len() as u64);

    let (alpha, beta): (Vec<&bls::G2Projective>, Vec<&PointList>) =
        verify_keys.iter().map(|&(ref a, ref b)| (a, b)).unzip();

    let attributes_size = beta[0].len();

    assert_eq!(lagrange.len(), alpha.len());

    let aggregate_alpha: bls::G2Projective = ec_sum(
        &alpha.iter().zip(lagrange.iter()).map(|(a, l)| *a * l).collect());
    let aggregate_beta: PointList = (0..attributes_size).map(|i| ec_sum(
        &beta.iter().zip(lagrange.iter()).map(|(b, l)| b[i] * l).collect())).collect();

    return (aggregate_alpha, aggregate_beta)
}

fn test_ttp_keygen() {
    let (threshold, number_authorities) = (5, 7);

    let mut rng = rand::thread_rng();

    let mut parameters = Parameters::new(2, &mut rng);

    let (secret_keys, verify_keys) = ttp_keygen(&mut parameters, threshold, number_authorities);

    let verify_key = aggregate_keys(&parameters, &verify_keys);

    let sigs_x: Vec<bls::G1Projective> = secret_keys.iter()
        .map(|(x, _)| parameters.g1 * x)
        .collect();
    let l = lagrange_basis(6);
    let sig = ecc_sum(&l.iter().zip(sigs_x.iter()).map(|(l_i, s_i)| s_i * l_i).collect());

    let ppair_1 = bls::pairing(&bls::G1Affine::from(sig), &parameters.g2);
    let ppair_2 = bls::pairing(&parameters.g1, &bls::G2Affine::from(verify_key.0));
    assert_eq!(ppair_1, ppair_2);
}

pub fn elgamal_keygen(params: &mut Parameters) -> (bls::Scalar, bls::G1Projective) {
    let d = params.random_scalar();
    (d, params.g1 * d)
}

type EncryptedValue = (bls::G1Projective, bls::G1Projective);

fn elgamal_encrypt(params: &mut Parameters, gamma: &bls::G1Projective,
                   attribute: &bls::Scalar, commit_hash: &bls::G1Projective,
                   attribute_key: &bls::Scalar)
    -> EncryptedValue {
    (params.g1 * attribute_key, gamma * attribute_key + commit_hash * attribute)
}

type AttributeList = Vec<bls::Scalar>;
type LambdaType = (bls::G1Projective, Vec<EncryptedValue>, SignerProof);

pub fn compute_commit_hash(attribute_commit: &bls::G1Projective) -> bls::G1Projective {
    let commit_data = bls::G1Affine::from(attribute_commit).to_compressed();
    let commit_hash = bls::G1Projective::hash_to_point(&commit_data);
    commit_hash
}

fn compute_challenge(points_g1: Vec<&bls::G1Projective>, points_g2: Vec<&bls::G2Projective>)
    -> bls::Scalar {
    for i in 0u32.. {
        let mut hasher = Sha256::new();

        let i_data = i.to_le_bytes();
        hasher.input(&i_data);

        for point in &points_g1 {
            let data = bls::G1Affine::from(*point).to_compressed();
            hasher.input(&data[0..32]);
            hasher.input(&data[32..]);
        }
        for point in &points_g2 {
            let data = bls::G2Affine::from(*point).to_compressed();
            hasher.input(&data[0..32]);
            hasher.input(&data[32..64]);
            hasher.input(&data[64..]);
        }
        let hash_result = hasher.result();

        // TODO: how can I fix this? Why not &hash_result[0...32]??
        let mut hash_data = [0u8; 32];
        hash_data.copy_from_slice(hash_result.as_slice());

        let challenge = bls::Scalar::from_bytes(&hash_data);
        if challenge.is_some().unwrap_u8() == 1 {
            return challenge.unwrap();
        }
    }
    unreachable!();
}

type SignerProof = (bls::Scalar, bls::Scalar, Vec<bls::Scalar>, Vec<bls::Scalar>);
type VerifyProof = (bls::Scalar, Vec<bls::Scalar>, bls::Scalar);

fn make_signer_proof(params: &mut Parameters, gamma: &bls::G1Projective,
                     ciphertext: &Vec<EncryptedValue>, attribute_commit: &bls::G1Projective,
                     commit_hash: &bls::G1Projective, attribute_keys: &Vec<bls::Scalar>,
                     attributes: &AttributeList, blinding_factor: &bls::Scalar)
    -> SignerProof {
    assert_eq!(ciphertext.len(), attribute_keys.len());
    assert_eq!(ciphertext.len(), attributes.len());

    // Random witness
    let witness_blind = params.random_scalar();
    let witness_keys: Vec<_> = attribute_keys.iter().map(|_| params.random_scalar()).collect();
    let witness_attributes: Vec<_> = attributes.iter().map(|_| params.random_scalar()).collect();

    // Witness commit
    let witness_commit_a: Vec<_> =
        witness_keys.iter().map(|witness| params.g1 * witness).collect();
    let witness_commit_b: Vec<_> =
        witness_keys.iter().zip(witness_attributes.iter())
            .map(|(witness_key, witness_attribute)|
                 gamma * witness_key + commit_hash * witness_attribute).collect();
    assert_eq!(witness_attributes.len(), params.hs.len());
    let witness_commit_attributes =
        params.g1 * witness_blind + ecc_sum(
            &params.hs.iter().zip(witness_attributes.iter())
                .map(|(h, witness)| h * witness).collect());


    // Challenge
    let g1 = bls::G1Projective::from(params.g1);
    let hs: Vec<_> = params.hs.iter().map(|h| bls::G1Projective::from(h)).collect();
    let challenge = compute_challenge(
        {
            let mut points: Vec<&_> = vec![
                &g1,                                    // G1
                attribute_commit,                       // C_m
                commit_hash,                            // h
                &witness_commit_attributes              // Cw
            ];
            points.extend(witness_commit_a.iter());     // Aw
            points.extend(witness_commit_b.iter());     // Bw
            points.extend(hs.iter());                   // hs
            points
        },
        vec![&bls::G2Projective::from(params.g2)]       // G2
    );

    // Responses
    assert_eq!(witness_keys.len(), attribute_keys.len());
    assert_eq!(witness_attributes.len(), attributes.len());
    let response_blind = witness_blind - challenge * blinding_factor;
    let response_keys: Vec<_> =
        witness_keys.iter().zip(attribute_keys.iter())
            .map(|(witness, key)| witness - challenge * key)
            .collect();
    let response_attributes: Vec<_> =
        witness_attributes.iter().zip(attributes.iter())
            .map(|(witness, attribute)| witness - challenge * attribute)
            .collect();

    (challenge, response_blind, response_keys, response_attributes)
}

fn verify_signer_proof(params: &Parameters, gamma: &bls::G1Projective,
                       ciphertext: &Vec<EncryptedValue>,
                       attribute_commit: &bls::G1Projective, commit_hash: &bls::G1Projective,
                       proof: &SignerProof) -> bool {
    let (a_factors, b_factors): (Vec<&_>, Vec<&_>) =
        ciphertext.iter().map(|&(ref a, ref b)| (a, b)).unzip();
    let (challenge, response_blind, response_keys, response_attributes) = proof;

    // Recompute witness commitments
    assert_eq!(ciphertext.len(), response_keys.len());
    assert_eq!(a_factors.len(), response_keys.len());
    assert_eq!(b_factors.len(), response_keys.len());
    let witness_commit_a: Vec<_> =
        a_factors.iter().zip(response_keys.iter())
            .map(|(a_i, response)| *a_i * challenge + params.g1 * response).collect();
    let witness_commit_b: Vec<_> =
        b_factors.iter().zip(response_keys.iter()).zip(response_attributes.iter())
            .map(|((b_i, response_key), response_attribute)|
                 *b_i * challenge + gamma * response_key + commit_hash * response_attribute)
            .collect();
    let witness_commit_attributes =
        attribute_commit * challenge + params.g1 * response_blind + ecc_sum(
            &params.hs.iter().zip(response_attributes.iter())
                .map(|(h_i, response)| h_i * response).collect());

    // Challenge
    let g1 = bls::G1Projective::from(params.g1);
    let hs: Vec<_> = params.hs.iter().map(|h| bls::G1Projective::from(h)).collect();
    let recomputed_challenge = compute_challenge(
        {
            let mut points: Vec<&_> = vec![
                &g1,                                    // G1
                attribute_commit,                       // C_m
                commit_hash,                            // h
                &witness_commit_attributes              // Cw
            ];
            points.extend(witness_commit_a.iter()); // Aw
            points.extend(witness_commit_b.iter()); // Bw
            points.extend(hs.iter());               // hs
            points
        },
        vec![&bls::G2Projective::from(params.g2)]       // G2
    );

    *challenge == recomputed_challenge
}

fn make_verify_proof(params: &mut Parameters, verify_key: &(bls::G2Projective, PointList),
                     blind_commit_hash: &bls::G1Projective, attributes: &Vec<bls::Scalar>,
                     blind: &bls::Scalar) -> VerifyProof
{
    let (alpha, beta) = verify_key;

    // Random witness
    let witness_kappa: Vec<_> = attributes.iter().map(|_| params.random_scalar()).collect();
    let witness_blind = params.random_scalar();

    // Witness commit
    assert_eq!(witness_kappa.len(), beta.len());
    let witness_commit_kappa = params.g2 * witness_blind + alpha + ecc_sum(
        &witness_kappa.iter().zip(beta.iter()).map(|(witness, beta_i)| beta_i * witness).collect()
    );
    let witness_commit_blind = blind_commit_hash * witness_blind;

    // Challenge
    let g1 = bls::G1Projective::from(params.g1);
    let g2 = bls::G2Projective::from(params.g2);
    let hs: Vec<_> = params.hs.iter().map(|h| bls::G1Projective::from(h)).collect();
    let challenge = compute_challenge(
        {
            let mut points: Vec<&_> = vec![
                &g1,                                    // G1
                &witness_commit_blind,                  // Bw
            ];
            points.extend(hs.iter());                   // hs
            points
        },
        {
            let mut points: Vec<&_> = vec![
                &g2,                                    // G2
                alpha,                                  // alpha
                &witness_commit_kappa                   // Aw
            ];
            points.extend(beta.iter());                 // beta
            points
        }
    );

    // Responses
    assert_eq!(witness_kappa.len(), attributes.len());
    let response_kappa: Vec<_> =
        witness_kappa.iter().zip(attributes.iter())
            .map(|(witness, attribute)| witness - challenge * attribute)
            .collect();
    let response_blind = witness_blind - challenge * blind;
    (challenge, response_kappa, response_blind)
}

fn verify_verify_proof(params: &Parameters, verify_key: &(bls::G2Projective, PointList),
                       blind_commit_hash: &bls::G1Projective,
                       kappa: &bls::G2Projective, v: &bls::G1Projective,
                       proof: &VerifyProof) -> bool {
    let (alpha, beta) = verify_key;
    let (challenge, response_kappa, response_blind) = proof;

    // Recompute witness commitments
    let witness_commit_kappa = kappa * challenge + params.g2 * response_blind
        + alpha * (bls::Scalar::one() - challenge)
        + ecc_sum(
            &beta.iter().zip(response_kappa.iter())
                .map(|(beta_i, response)| beta_i * response).collect());
    let witness_commit_blind = v * challenge + blind_commit_hash * response_blind;

    // Challenge
    let g1 = bls::G1Projective::from(params.g1);
    let g2 = bls::G2Projective::from(params.g2);
    let hs: Vec<_> = params.hs.iter().map(|h| bls::G1Projective::from(h)).collect();
    let recomputed_challenge = compute_challenge(
        {
            let mut points: Vec<&_> = vec![
                &g1,                                    // G1
                &witness_commit_blind,                  // Bw
            ];
            points.extend(hs.iter());                   // hs
            points
        },
        {
            let mut points: Vec<&_> = vec![
                &g2,                                    // G2
                alpha,                                  // alpha
                &witness_commit_kappa                   // Aw
            ];
            points.extend(beta.iter());                 // beta
            points
        }
    );

    *challenge == recomputed_challenge
}

pub fn prepare_blind_sign(params: &mut Parameters, gamma: &bls::G1Projective,
                      attributes: &AttributeList) -> LambdaType {
    let blinding_factor = params.random_scalar();
    assert_eq!(params.hs.len(), attributes.len());
    let attribute_commit =
        params.g1 * blinding_factor +
        ecc_sum(
            &params.hs.iter()
            .zip(attributes.iter())
            .map(|(h_generator, attribute)| h_generator * attribute)
            .collect()
        );
    let commit_hash = compute_commit_hash(&attribute_commit);

    let attribute_keys: Vec<_> =
        (0..attributes.len()).map(|_| params.random_scalar()).collect();
    let encrypted_attributes: Vec<(_, _)> =
        attributes.iter().zip(attribute_keys.iter())
            .map(|(attribute, key)|
                elgamal_encrypt(params, gamma, &attribute, &commit_hash, &key))
            .collect();

    let signer_proof = make_signer_proof(params, gamma, &encrypted_attributes, &attribute_commit,
                                         &commit_hash, &attribute_keys, &attributes,
                                         &blinding_factor);

    (attribute_commit, encrypted_attributes, signer_proof)
}

type PartialSignature = (bls::G1Projective, bls::G1Projective);

pub fn blind_sign(params: &Parameters, secret_key: &SecretKey,
                  gamma: &bls::G1Projective, lambda: &LambdaType)
    -> Result<PartialSignature, &'static str> {
    let (x, y) = secret_key;
    let (attribute_commit, encrypted_attributes, signer_proof) = lambda;

    assert_eq!(encrypted_attributes.len(), params.hs.len());
    let (a_factors, b_factors): (Vec<&_>, Vec<&_>) =
        encrypted_attributes.iter().map(|&(ref a, ref b)| (a, b)).unzip();

    // Issue signature
    let commit_hash = compute_commit_hash(attribute_commit);

    // Verify proof here
    if !verify_signer_proof(params, &gamma, encrypted_attributes,
                            attribute_commit, &commit_hash, signer_proof) {
        return Err("verify proof failed")
    }

    // TODO: Add public attributes - need to see about selective reveal
    let signature_a = ecc_sum(
        &y.iter().zip(a_factors.iter())
            .map(|(y_j, a)| *a * y_j)
            .collect()
    );

    let signature_b = commit_hash * x
        + ecc_sum(
            &y.iter().zip(b_factors.iter())
                .map(|(y_j, b)| *b * y_j)
                .collect()
        );

    Ok((signature_a, signature_b))
}

fn elgamal_decrypt(private_key: &bls::Scalar, encrypted_value: &EncryptedValue)
    -> bls::G1Projective {
    let (a, b) = encrypted_value;
    b - a * private_key
}

pub fn unblind(private_key: &bls::Scalar, encrypted_value: &EncryptedValue)
    -> bls::G1Projective {
    elgamal_decrypt(private_key, encrypted_value)
}

fn lagrange_basis2(indexes: &Vec<u64>) -> ScalarList {
    let x = bls::Scalar::zero();
    let mut lagrange_result = ScalarList::new();
    for i in indexes {
        let mut numerator = bls::Scalar::one();
        let mut denominator = bls::Scalar::one();

        for j in indexes {
            if j == i {
                continue;
            }
            numerator = numerator * (x - bls::Scalar::from(*j));
            denominator = denominator * (bls::Scalar::from(*i) - bls::Scalar::from(*j));
        }

        let result = numerator * denominator.invert().unwrap();
        lagrange_result.push(result);
    }
    lagrange_result
}

pub fn aggregate_credential(signature_shares: &Vec<bls::G1Projective>, indexes: &Vec<u64>)
    -> bls::G1Projective {
    let lagrange = lagrange_basis2(indexes);

    let aggregate_shares = ecc_sum(
        &signature_shares.iter().zip(lagrange.iter())
            .map(|(signature_share, lagrange_i)| signature_share * lagrange_i)
            .collect()
    );
    aggregate_shares
}

pub fn prove_credential(params: &mut Parameters, verify_key: &(bls::G2Projective, PointList),
                    signature: &(bls::G1Projective, bls::G1Projective),
                    attributes: &Vec<bls::Scalar>)
    -> (bls::G2Projective, bls::G1Projective,
        (bls::G1Projective, bls::G1Projective), VerifyProof) {
    let (alpha, beta) = verify_key;
    let (commit_hash, sigma) = signature;
    assert_eq!(attributes.len(), beta.len());

    let blind_prime = params.random_scalar();
    let (blinded_commit_hash, blinded_sigma) = (commit_hash * blind_prime, sigma * blind_prime);

    let blind = params.random_scalar();

    let kappa = params.g2 * blind + alpha + ecc_sum(
        &beta.iter().zip(attributes.iter())
            .map(|(beta_i, attribute)| beta_i * attribute)
            .collect()
    );
    let v = blinded_commit_hash * blind;

    let proof = make_verify_proof(params, verify_key, &blinded_commit_hash, attributes, &blind);

    (kappa, v, (blinded_commit_hash, blinded_sigma), proof)
}

pub fn verify_credential(params: &Parameters, verify_key: &(bls::G2Projective, PointList),
                         proven_credential: &(bls::G2Projective, bls::G1Projective,
                                            (bls::G1Projective, bls::G1Projective),
                                            VerifyProof)) -> bool
{
    //let (_, beta) = verify_key;
    let (kappa_projective, v, (blind_commit_projective, blinded_sigma), proof) = proven_credential;
    if !verify_verify_proof(params, verify_key, blind_commit_projective, kappa_projective,
                            v, proof) {
        return false
    }
    let kappa = bls::G2Affine::from(kappa_projective);
    let blind_commit = bls::G1Affine::from(blind_commit_projective);
    let sigma_nu = bls::G1Affine::from(blinded_sigma + v);
    bls::pairing(&blind_commit, &kappa) == bls::pairing(&sigma_nu, &params.g2)
}

#[test]
fn it_works() {
    test_ttp_keygen();

    let (threshold, number_authorities) = (5, 7);

    let mut rng = rand::thread_rng();
    let mut parameters = Parameters::new(2, &mut rng);

    let (secret_keys, verify_keys) = ttp_keygen(&mut parameters, threshold, number_authorities);
    let verify_key = aggregate_keys(&parameters, &verify_keys);

    let (d, gamma) = elgamal_keygen(&mut parameters);

    let attributes = vec![bls::Scalar::from(110), bls::Scalar::from(4)];

    let lambda = prepare_blind_sign(&mut parameters, &gamma, &attributes);

    let blind_signatures: Vec<_> =
        secret_keys.iter()
            .map(|secret_key| blind_sign(&parameters, secret_key, &gamma, &lambda).unwrap())
            .collect();

    // Signatures should be a struct, with an authority ID inside them
    let mut signature_shares: Vec<_> =
        blind_signatures.iter()
            .map(|blind_signature| unblind(&d, blind_signature))
            .collect();
    let mut indexes: Vec<u64> = (1u64..=signature_shares.len() as u64).collect();

    signature_shares.remove(0);
    indexes.remove(0);
    signature_shares.remove(4);
    indexes.remove(4);

    let commit_hash = compute_commit_hash(&lambda.0);
    let signature = (commit_hash, aggregate_credential(&signature_shares, &indexes));

    let proven_credential =
        prove_credential(&mut parameters, &verify_key, &signature, &attributes);

    let is_verify = verify_credential(&parameters, &verify_key, &proven_credential);
    assert!(is_verify);

    ///////////

    let g1 = bls::G1Affine::generator();
    let g2 = bls::G2Affine::generator();

    let x = bls::Scalar::new_random(&mut rng);

    let result_1 = bls::G1Affine::from(g1 * x);
    let result_2 = bls::G2Affine::from(g2 * x);

    let pair_1 = bls::pairing(&result_1, &g2);
    let pair_2 = bls::pairing(&g1, &result_2);

    let foo = bls::G1Affine::hash_to_point(b"hello world");

    assert_eq!(pair_1, pair_2);

    println!("{:?}", x);
    println!("{:?}", result_1);
    println!("{:?}", pair_1);

    //let pairx = pairing(&g1, &g2);

    /*
    // Generate private keys
    let alice_sk = Fr::random(&mut rng);
    let bob_sk = Fr::random(&mut rng);
    let carol_sk = Fr::random(&mut rng);

    // Generate public keys in G1 and G2
    let (alice_pk1, alice_pk2) = (G1::one() * alice_sk, G2::one() * alice_sk);
    let (bob_pk1, bob_pk2) = (G1::one() * bob_sk, G2::one() * bob_sk);
    let (carol_pk1, carol_pk2) = (G1::one() * carol_sk, G2::one() * carol_sk);

    // Each party computes the shared secret
    let alice_ss = pairing(bob_pk1, carol_pk2).pow(alice_sk);
    let bob_ss = pairing(carol_pk1, alice_pk2).pow(bob_sk);
    let carol_ss = pairing(alice_pk1, bob_pk2).pow(carol_sk);

    assert!(alice_ss == bob_ss && bob_ss == carol_ss);
    */

    println!("Hello, world!");
}
